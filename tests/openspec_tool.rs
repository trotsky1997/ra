//! Integration tests for the native `openspec` tool. These use a fake
//! `openspec` binary on `PATH` so the tests verify Ra's argv/cwd/envelope
//! behavior without depending on a real OpenSpec install.

use ra::{
    tools::{OpenSpecTool, Tool},
    ToolCtx,
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
    let (events, _) = tokio::sync::broadcast::channel(16);
    ToolCtx::local(events)
}

fn json_output(output: &str) -> Value {
    serde_json::from_str(output).unwrap_or_else(|err| panic!("invalid json: {err}: {output}"))
}

/// Write a fake `openspec` that records its argv (one per line) to
/// `$OPENSPEC_ARGS_FILE` and then runs `body` (which controls stdout/stderr
/// and exit code).
fn write_fake_openspec(dir: &Path, body: &str) {
    let path = dir.join("openspec");
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$OPENSPEC_ARGS_FILE\"\n{body}\n"),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

#[tokio::test]
async fn default_catalog_contains_openspec_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"openspec".to_string()),
        "missing openspec: {names:?}"
    );

    let filtered = ra::default_builtins(&["openspec".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["openspec"]);
}

#[tokio::test]
async fn status_passes_change_and_json_and_appends_no_color() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_openspec(dir.path(), "printf '{\\\"artifacts\\\":[]}\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args = EnvRestore::set("OPENSPEC_ARGS_FILE", args_file.as_os_str());

    let output = OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({ "action": "status", "change": "add-thing" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let args = fs::read_to_string(&args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec!["status", "--change", "add-thing", "--json", "--no-color"]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "openspec");
    assert_eq!(output["action"], "status");
    assert_eq!(output["exit_code"], 0);
}

#[tokio::test]
async fn workflow_state_derives_summary_from_status_json() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let stdout_file = dir.path().join("status.json");
    fs::write(
        &stdout_file,
        r#"{"changeName":"c","isComplete":false,"applyRequires":["tasks"],"artifacts":[{"id":"proposal","status":"done"},{"id":"tasks","status":"ready"}]}"#,
    )
    .unwrap();
    // Emit the status JSON verbatim from a file so shell quoting can't mangle it.
    write_fake_openspec(dir.path(), "/bin/cat \"$OPENSPEC_STDOUT_FILE\"\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args = EnvRestore::set("OPENSPEC_ARGS_FILE", args_file.as_os_str());
    let _stdout = EnvRestore::set("OPENSPEC_STDOUT_FILE", stdout_file.as_os_str());

    let output = OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({ "action": "workflow_state", "change": "c" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    // workflow_state runs `status --json` under the hood.
    let args = fs::read_to_string(&args_file).unwrap();
    assert_eq!(args.lines().next(), Some("status"));

    let output = json_output(&output);
    let summary = &output["workflow_state"];
    assert_eq!(summary["ready"], serde_json::json!(["tasks"]));
    assert_eq!(summary["done"], serde_json::json!(["proposal"]));
    assert_eq!(summary["applyReady"], false);
}

#[tokio::test]
async fn archive_without_confirmation_never_spawns_openspec() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_openspec(dir.path(), "printf 'should-not-run\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args = EnvRestore::set("OPENSPEC_ARGS_FILE", args_file.as_os_str());

    let output = OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({ "action": "archive", "change": "add-thing" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    assert!(
        !args_file.exists(),
        "openspec must not be spawned when archive is unconfirmed"
    );
    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["error"]["kind"], "invalid_request");
    assert!(output["error"]["message"]
        .as_str()
        .unwrap()
        .contains("confirm_archive"));
}

#[tokio::test]
async fn archive_rejects_leading_dash_change_without_spawning() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_openspec(dir.path(), "printf 'should-not-run\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args = EnvRestore::set("OPENSPEC_ARGS_FILE", args_file.as_os_str());

    // The reviewer's exploit: a "change" of `--skip-specs` would otherwise be
    // parsed as a flag on the destructive archive command. It must be rejected
    // before the binary is ever spawned, even with confirm_archive set.
    let output = OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({
                "action": "archive",
                "change": "--skip-specs",
                "confirm_archive": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    assert!(
        !args_file.exists(),
        "openspec must not be spawned for an unsafe change name"
    );
    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["error"]["kind"], "invalid_request");
    assert!(output["error"]["message"]
        .as_str()
        .unwrap()
        .contains("must not start with `-`"));
}

#[tokio::test]
async fn archive_with_confirmation_passes_yes() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_openspec(dir.path(), "printf 'archived\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args = EnvRestore::set("OPENSPEC_ARGS_FILE", args_file.as_os_str());

    OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({
                "action": "archive",
                "change": "add-thing",
                "confirm_archive": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let args = fs::read_to_string(&args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec!["archive", "add-thing", "-y", "--no-color"]
    );
}

#[tokio::test]
async fn non_zero_exit_is_surfaced_as_error_envelope() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_openspec(dir.path(), "printf 'boom\\n' 1>&2\nexit 3\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args = EnvRestore::set("OPENSPEC_ARGS_FILE", args_file.as_os_str());

    let output = OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({ "action": "list" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], 3);
    assert_eq!(output["error"]["kind"], "openspec_error");
    assert_eq!(output["stderr"], "boom\n");
}

#[tokio::test]
async fn missing_binary_returns_structured_guidance() {
    let _guard = env_lock().lock().await;
    // An empty PATH dir guarantees no openspec binary is found.
    let dir = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());

    let output = OpenSpecTool
        .execute(
            "openspec",
            serde_json::json!({ "action": "status", "change": "c" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["error"]["kind"], "missing_openspec");
}
