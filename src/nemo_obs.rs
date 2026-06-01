//! Thin observability adapter over NeMo-Relay.
//!
//! Goal: surface Ra's runtime activity as ATOF (Agent Trajectory
//! Observability Format) events so that any NeMo-Relay-aware backend
//! (OpenTelemetry / OpenInference / file dispatcher / custom) can pick
//! it up. We do **not** rebuild the session lifecycle on top of NeMo —
//! Ra still owns the turn loop, the broadcast bus, the ATIF file format
//! on disk. NeMo is a side-channel for telemetry.

use std::sync::Arc;

use nemo_relay::api::event::Event as AtofEvent;
use nemo_relay::api::runtime::{create_scope_stack, TASK_SCOPE_STACK};
use nemo_relay::api::scope::{
    event, pop_scope, push_scope, EmitMarkEventParams, PopScopeParams, PushScopeParams,
    ScopeHandle, ScopeType,
};
use nemo_relay::api::subscriber::register_subscriber;

/// One-time process-global init. Called from `acp_server::run` and idempotent
/// — second invocation is a noop because the subscriber name collides.
///
/// Backend selection (`RA_OBS_BACKEND` env var):
///   `stderr` (default) — one-line JSON per ATOF event on stderr.
///   `file`             — append JSONL at $RA_HOME/obs/atof-<pid>.jsonl
///                        (or $XDG_DATA_HOME/ra/obs/...).
///   `otel`             — OpenTelemetry OTLP HTTP exporter; reads
///                        $OTEL_EXPORTER_OTLP_ENDPOINT and friends.
///   `none`             — register nothing.
pub fn init() {
    let backend = std::env::var("RA_OBS_BACKEND").unwrap_or_else(|_| "stderr".into());
    match backend.as_str() {
        "none" => {
            eprintln!("[ra::atof] backend=none (observability disabled)");
        }
        "file" => init_file(),
        "otel" => init_otel(),
        _ => init_stderr(),
    }
}

fn init_stderr() {
    let cb: Arc<dyn Fn(&AtofEvent) + Send + Sync> =
        Arc::new(|event: &AtofEvent| match event.try_to_json_value() {
            Ok(v) => eprintln!("[ra::atof] {}", v),
            Err(e) => eprintln!("[ra::atof] <serialize error: {e}>"),
        });
    if let Err(e) = register_subscriber("ra-stderr", cb) {
        eprintln!("[ra::atof] register_subscriber: {e}");
    } else {
        eprintln!("[ra::atof] backend=stderr");
    }
}

fn init_file() {
    use std::io::Write;
    use std::sync::Mutex;

    let dir = match std::env::var_os("RA_HOME") {
        Some(p) => std::path::PathBuf::from(p).join("obs"),
        None => match dirs::data_local_dir() {
            Some(d) => d.join("ra/obs"),
            None => {
                eprintln!("[ra::atof] backend=file: cannot resolve data_local_dir; falling back to stderr");
                init_stderr();
                return;
            }
        },
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "[ra::atof] backend=file: mkdir {}: {e}; falling back to stderr",
            dir.display()
        );
        init_stderr();
        return;
    }
    let path = dir.join(format!("atof-{}.jsonl", std::process::id()));
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "[ra::atof] backend=file: open {}: {e}; falling back to stderr",
                path.display()
            );
            init_stderr();
            return;
        }
    };
    let writer = Arc::new(Mutex::new(std::io::BufWriter::new(file)));
    let writer_for_cb = writer.clone();
    let cb: Arc<dyn Fn(&AtofEvent) + Send + Sync> = Arc::new(move |event: &AtofEvent| {
        if let Ok(v) = event.try_to_json_value() {
            if let Ok(mut w) = writer_for_cb.lock() {
                let _ = serde_json::to_writer(&mut *w, &v);
                let _ = w.write_all(b"\n");
                // Flush once per event so external tailers see fresh data.
                let _ = w.flush();
            }
        }
    });
    if let Err(e) = register_subscriber("ra-file", cb) {
        eprintln!("[ra::atof] register_subscriber: {e}");
    } else {
        eprintln!("[ra::atof] backend=file path={}", path.display());
    }
}

fn init_otel() {
    use nemo_relay::observability::otel::{OpenTelemetryConfig, OpenTelemetrySubscriber};

    let cfg = OpenTelemetryConfig::http_binary("ra");
    let subscriber = match OpenTelemetrySubscriber::new(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ra::atof] backend=otel: init failed: {e}; falling back to stderr");
            init_stderr();
            return;
        }
    };
    let cb = subscriber.subscriber();
    // Stash the subscriber so OTel's processor task isn't dropped (which
    // would shut down the exporter). Leak it for the lifetime of the
    // process — the agent only ever has one observability backend, and it
    // lives until exit.
    Box::leak(Box::new(subscriber));
    if let Err(e) = register_subscriber("ra-otel", cb) {
        eprintln!("[ra::atof] register_subscriber: {e}");
    } else {
        eprintln!(
            "[ra::atof] backend=otel endpoint={}",
            std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4318 (default)".into())
        );
    }
}

/// Push an Agent-typed scope. Caller drops the returned guard to pop it.
/// Errors are logged and the guard becomes a no-op so observability never
/// crashes the request path.
pub fn agent_scope(name: &str) -> ScopeGuard {
    let handle = push_scope(
        PushScopeParams::builder()
            .name(name)
            .scope_type(ScopeType::Agent)
            .build(),
    )
    .map_err(|e| eprintln!("[ra::atof] push_scope agent {name}: {e}"))
    .ok();
    ScopeGuard { handle }
}

/// Push a Tool-typed scope around a single tool execution.
pub fn tool_scope(name: &str) -> ScopeGuard {
    let handle = push_scope(
        PushScopeParams::builder()
            .name(name)
            .scope_type(ScopeType::Tool)
            .build(),
    )
    .map_err(|e| eprintln!("[ra::atof] push_scope tool {name}: {e}"))
    .ok();
    ScopeGuard { handle }
}

/// Push an LLM-typed scope around a single model.stream() call.
pub fn llm_scope(name: &str) -> ScopeGuard {
    let handle = push_scope(
        PushScopeParams::builder()
            .name(name)
            .scope_type(ScopeType::Llm)
            .build(),
    )
    .map_err(|e| eprintln!("[ra::atof] push_scope llm {name}: {e}"))
    .ok();
    ScopeGuard { handle }
}

/// Emit a one-shot mark event under the current scope. Cheap; safe to call
/// from hot paths.
pub fn mark(name: &str) {
    if let Err(e) = event(EmitMarkEventParams::builder().name(name).build()) {
        eprintln!("[ra::atof] mark {name}: {e}");
    }
}

/// RAII guard that pops its scope on drop. Errors during pop are logged.
/// Cheap to drop in error paths; if the push failed we just don't pop.
pub struct ScopeGuard {
    handle: Option<ScopeHandle>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        if let Err(e) = pop_scope(PopScopeParams::builder().handle_uuid(&handle.uuid).build()) {
            eprintln!("[ra::atof] pop_scope: {e}");
        }
    }
}

/// Wrap a future so that all `push_scope` / `pop_scope` calls inside it see
/// the same task-local scope stack — even across `.await` points that may
/// migrate the task between worker threads.
///
/// Required at the boundary of any `tokio::spawn` whose body uses
/// scope helpers; otherwise pop will be looking at a different stack
/// than push, and you'll get "scope handle not found".
pub async fn with_task_scope<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    TASK_SCOPE_STACK.scope(create_scope_stack(), fut).await
}
