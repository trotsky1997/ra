//! Integration tests for native tmux tools.

use ra::{
    tools::{
        TmuxCaptureTool, TmuxKillTool, TmuxListenTool, TmuxRunTool, TmuxSendTool, TmuxWaitTool,
        Tool,
    },
    Event, ToolCtx,
};
use serde_json::Value;
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::Path, sync::OnceLock};
use tokio::sync::Mutex;

struct EnvRestore {
    key: &'static str,
    old_value: Option<OsString>,
}

impl EnvRestore {
    fn set<K: Into<OsString>>(key: &'static str, value: K) -> Self {
        let old_value = std::env::var_os(key);
        std::env::set_var(key, value.into());
        Self { key, old_value }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(value) = &self.old_value {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn make_ctx() -> ToolCtx {
    let (events, _) = tokio::sync::broadcast::channel(32);
    ToolCtx::local(events)
}

fn make_ctx_with_events() -> (ToolCtx, tokio::sync::broadcast::Receiver<Event>) {
    let (events, rx) = tokio::sync::broadcast::channel(32);
    (ToolCtx::local(events), rx)
}

fn json_output(output: &str) -> Value {
    serde_json::from_str(output).unwrap_or_else(|err| panic!("invalid json: {err}: {output}"))
}

fn write_fake_tmux(dir: &Path, body: &str) {
    let path = dir.join("tmux");
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$TMUX_ARGS_FILE\"\n{body}\n"),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn default_catalog_contains_tmux_tools_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    for name in [
        "tmux_run",
        "tmux_send",
        "tmux_capture",
        "tmux_kill",
        "tmux_listen",
        "tmux_wait",
    ] {
        assert!(
            names.contains(&name.to_string()),
            "missing {name}: {names:?}"
        );
    }

    let filtered = ra::default_builtins(&["tmux_capture".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["tmux_capture"]);
}

#[tokio::test]
async fn wait_requires_timeout_in_schema_params() {
    let err = TmuxWaitTool
        .execute(
            "tw",
            serde_json::json!({
                "event": "sleep",
                "duration_ms": 1
            }),
            &make_ctx(),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("invalid params for tmux_wait"));
}

#[tokio::test]
async fn capture_returns_structured_missing_tmux_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = TmuxCaptureTool
        .execute(
            "tc",
            serde_json::json!({ "session": "dev", "start_line": -50 }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["tool"], "tmux_capture");
    assert_eq!(output["error"]["kind"], "missing_tmux");
    assert!(output["error"]["install"][0]
        .as_str()
        .unwrap()
        .contains("Install tmux"));
}

#[tokio::test]
async fn wait_sleep_respects_timeout() {
    let output = TmuxWaitTool
        .execute(
            "tw",
            serde_json::json!({
                "event": "sleep",
                "duration_ms": 50,
                "timeout_ms": 5
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["tool"], "tmux_wait");
    assert_eq!(output["event"]["kind"], "sleep");
    assert_eq!(output["timed_out"], true);
    assert_eq!(output["triggered"], false);
}

#[tokio::test]
async fn send_executes_fake_tmux_with_expected_argv_and_events() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_tmux(dir.path(), "exit 0");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("TMUX_ARGS_FILE", args_file.as_os_str());
    let (ctx, mut rx) = make_ctx_with_events();

    let output = TmuxSendTool
        .execute(
            "ts",
            serde_json::json!({
                "session": "dev",
                "window": "repl",
                "keys": "hello; rm -rf /",
                "enter": true
            }),
            &ctx,
        )
        .await
        .unwrap();

    let args = fs::read_to_string(args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "send-keys",
            "-t",
            "ra__dev:repl",
            "-l",
            "--",
            "hello; rm -rf /",
            "send-keys",
            "-t",
            "ra__dev:repl",
            "Enter",
        ]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["target"]["session"], "ra__dev");
    assert_eq!(output["target"]["target"], "ra__dev:repl");

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ToolCallUpdate { chunk, .. } if chunk.contains("[tmux] tmux send-keys")
    )));
}

#[tokio::test]
async fn capture_executes_fake_tmux_with_line_bounds_and_truncates_output() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_tmux(dir.path(), "printf '%02000d\\n' 0\nexit 0");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("TMUX_ARGS_FILE", args_file.as_os_str());

    let output = TmuxCaptureTool
        .execute(
            "tc",
            serde_json::json!({
                "session": "dev",
                "window": "logs",
                "start_line": -10,
                "end_line": -1,
                "max_output_bytes": 200
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let args = fs::read_to_string(args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "capture-pane",
            "-p",
            "-t",
            "ra__dev:logs",
            "-S",
            "-10",
            "-E",
            "-1",
        ]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["truncated"], true);
    assert!(output["stdout"].as_str().unwrap().contains("[truncated]"));
}

#[tokio::test]
async fn listen_detects_pattern_from_fake_tmux() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let counter_file = dir.path().join("counter.txt");
    write_fake_tmux(
        dir.path(),
        &format!(
            "if [ -f {} ]; then IFS= read -r n < {}; else n=0; fi\n\
             n=$((n + 1))\n\
             printf '%s' \"$n\" > {}\n\
             if [ \"$n\" -ge 2 ]; then printf 'ready\\n'; else printf 'booting\\n'; fi\n\
             exit 0",
            counter_file.display(),
            counter_file.display(),
            counter_file.display()
        ),
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("TMUX_ARGS_FILE", args_file.as_os_str());

    let output = TmuxListenTool
        .execute(
            "tl",
            serde_json::json!({
                "session": "dev",
                "pattern": "ready",
                "timeout_ms": 1000,
                "poll_ms": 10
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["matched"], true);
    assert_eq!(output["timed_out"], false);
    assert!(output["stdout"].as_str().unwrap().contains("ready"));
}

#[tokio::test]
async fn listen_and_wait_share_hook_expression_semantics() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let counter_file = dir.path().join("counter.txt");
    write_fake_tmux(
        dir.path(),
        &format!(
            "if [ -f {} ]; then IFS= read -r n < {}; else n=0; fi\n\
             n=$((n + 1))\n\
             printf '%s' \"$n\" > {}\n\
             if [ \"$n\" -ge 2 ]; then printf 'HOOK_READY job=42\\n'; else printf 'idle\\n'; fi\n\
             exit 0",
            counter_file.display(),
            counter_file.display(),
            counter_file.display()
        ),
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("TMUX_ARGS_FILE", args_file.as_os_str());

    let listen = TmuxListenTool
        .execute(
            "tl",
            serde_json::json!({
                "session": "dev",
                "event": "hook",
                "hook": "ready",
                "pattern": "HOOK_READY job=\\d+",
                "regex": true,
                "timeout_ms": 1000,
                "poll_ms": 10
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let listen = json_output(&listen);
    assert_eq!(listen["ok"], true);
    assert_eq!(listen["event"]["kind"], "hook");
    assert_eq!(listen["event"]["hook"], "ready");
    assert_eq!(listen["matched"], true);

    fs::write(&counter_file, "0").unwrap();
    let wait = TmuxWaitTool
        .execute(
            "tw",
            serde_json::json!({
                "session": "dev",
                "event": "hook",
                "hook": "ready",
                "pattern": "HOOK_READY job=\\d+",
                "regex": true,
                "timeout_ms": 1000,
                "poll_ms": 10
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let wait = json_output(&wait);
    assert_eq!(wait["ok"], true);
    assert_eq!(wait["event"]["kind"], "hook");
    assert_eq!(wait["event"]["hook"], "ready");
    assert_eq!(wait["matched"], true);
}

#[tokio::test]
async fn wait_detects_output_update_from_fake_tmux() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let counter_file = dir.path().join("counter.txt");
    write_fake_tmux(
        dir.path(),
        &format!(
            "if [ -f {} ]; then IFS= read -r n < {}; else n=0; fi\n\
             n=$((n + 1))\n\
             printf '%s' \"$n\" > {}\n\
             if [ \"$n\" -ge 2 ]; then printf 'new output\\n'; else printf 'old output\\n'; fi\n\
             exit 0",
            counter_file.display(),
            counter_file.display(),
            counter_file.display()
        ),
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("TMUX_ARGS_FILE", args_file.as_os_str());

    let output = TmuxWaitTool
        .execute(
            "tw",
            serde_json::json!({
                "session": "dev",
                "event": "output_update",
                "timeout_ms": 1000,
                "poll_ms": 10
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "tmux_wait");
    assert_eq!(output["event"]["kind"], "output_update");
    assert_eq!(output["changed"], true);
    assert_eq!(output["timed_out"], false);
    assert!(output["stdout"].as_str().unwrap().contains("new output"));
}

#[tokio::test]
async fn real_tmux_round_trip_run_capture_send_listen_and_kill() {
    let _guard = env_lock().lock().await;
    if which::which("tmux").is_err() {
        eprintln!("skipping real tmux round-trip test: tmux not on PATH");
        return;
    }

    let session = format!("test{}", std::process::id());
    let ctx = make_ctx();

    let run = TmuxRunTool
        .execute(
            "tr",
            serde_json::json!({
                "session": session,
                "window": "main",
                "command": "printf 'alpha\\n'",
                "wait": true,
                "timeout_ms": 5000,
                "max_output_bytes": 10000
            }),
            &ctx,
        )
        .await
        .unwrap();
    let run = json_output(&run);
    assert_eq!(run["ok"], true, "run={run}");
    assert!(
        run["stdout"].as_str().unwrap().contains("alpha"),
        "run={run}"
    );

    let capture = TmuxCaptureTool
        .execute(
            "tc",
            serde_json::json!({
                "session": session,
                "window": "main",
                "start_line": -20
            }),
            &ctx,
        )
        .await
        .unwrap();
    let capture = json_output(&capture);
    assert_eq!(capture["ok"], true, "capture={capture}");
    assert!(
        capture["stdout"].as_str().unwrap().contains("alpha"),
        "capture={capture}"
    );

    let _send = TmuxSendTool
        .execute(
            "ts",
            serde_json::json!({
                "session": session,
                "window": "main",
                "keys": "printf 'beta\\n'",
                "enter": true
            }),
            &ctx,
        )
        .await
        .unwrap();

    let listen = TmuxListenTool
        .execute(
            "tl",
            serde_json::json!({
                "session": session,
                "window": "main",
                "pattern": "beta",
                "timeout_ms": 5000,
                "poll_ms": 100,
                "start_line": -20
            }),
            &ctx,
        )
        .await
        .unwrap();
    let listen = json_output(&listen);
    assert_eq!(listen["ok"], true, "listen={listen}");
    assert_eq!(listen["matched"], true, "listen={listen}");

    let wait = TmuxWaitTool
        .execute(
            "tw",
            serde_json::json!({
                "session": session,
                "window": "main",
                "event": "program_output",
                "command": "printf 'gamma\\n'",
                "pattern": "gamma",
                "timeout_ms": 5000,
                "poll_ms": 100,
                "max_output_bytes": 10000
            }),
            &ctx,
        )
        .await
        .unwrap();
    let wait = json_output(&wait);
    assert_eq!(wait["ok"], true, "wait={wait}");
    assert_eq!(wait["event"]["kind"], "program_output", "wait={wait}");
    assert_eq!(wait["matched"], true, "wait={wait}");
    assert!(
        wait["stdout"].as_str().unwrap().contains("gamma"),
        "wait={wait}"
    );

    let wait_exit = TmuxWaitTool
        .execute(
            "twx",
            serde_json::json!({
                "session": session,
                "window": "main",
                "event": "program_exit",
                "command": "exit 3",
                "timeout_ms": 5000,
                "poll_ms": 100,
                "max_output_bytes": 10000
            }),
            &ctx,
        )
        .await
        .unwrap();
    let wait_exit = json_output(&wait_exit);
    assert_eq!(wait_exit["ok"], true, "wait_exit={wait_exit}");
    assert_eq!(wait_exit["command_exit_code"], 3, "wait_exit={wait_exit}");

    let kill = TmuxKillTool
        .execute("tk", serde_json::json!({ "session": session }), &ctx)
        .await
        .unwrap();
    let kill = json_output(&kill);
    assert_eq!(kill["ok"], true, "kill={kill}");
}
