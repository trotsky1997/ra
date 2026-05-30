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
use nemo_relay::api::runtime::{TASK_SCOPE_STACK, create_scope_stack};
use nemo_relay::api::scope::{
    EmitMarkEventParams, PopScopeParams, PushScopeParams, ScopeHandle, ScopeType, event,
    pop_scope, push_scope,
};
use nemo_relay::api::subscriber::register_subscriber;

/// One-time process-global init. Called from `acp_server::run` and idempotent
/// — second invocation is a noop because the subscriber name collides.
pub fn init() {
    let cb: Arc<dyn Fn(&AtofEvent) + Send + Sync> = Arc::new(|event: &AtofEvent| {
        // Compact JSON line per event. Standalone log channel; we
        // intentionally don't hook the `tracing` crate so user-installed
        // tracing subscribers can't accidentally swallow these.
        match event.try_to_json_value() {
            Ok(v) => eprintln!("[ra::atof] {}", v),
            Err(e) => eprintln!("[ra::atof] <serialize error: {e}>"),
        }
    });

    if let Err(e) = register_subscriber("ra-stderr", cb) {
        eprintln!("[ra::atof] register_subscriber: {e}");
    } else {
        eprintln!("[ra::atof] subscriber registered");
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
        if let Err(e) = pop_scope(
            PopScopeParams::builder()
                .handle_uuid(&handle.uuid)
                .build(),
        ) {
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

