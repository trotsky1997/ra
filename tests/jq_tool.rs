//! Integration tests for the native jq tool. These use a fake `jq` binary so
//! the tests verify Ra's argv/stdin/envelope behavior without depending on the
//! host jq installation.

use ra::{
    tools::{JqTool, Tool},
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

fn write_fake_jq(dir: &Path, body: &str) {
    let path = dir.join("jq");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$JQ_ARGS_FILE\"\n/bin/cat > \"$JQ_STDIN_FILE\"\n{body}\n"
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[tokio::test]
async fn default_catalog_contains_jq_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    assert!(names.contains(&"jq".to_string()), "missing jq: {names:?}");

    let filtered = ra::default_builtins(&["jq".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["jq"]);
}

#[tokio::test]
async fn inline_input_executes_fake_jq_with_expected_argv_and_stdin() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let stdin_file = dir.path().join("stdin.json");
    write_fake_jq(dir.path(), "printf 'Ada\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("JQ_ARGS_FILE", args_file.as_os_str());
    let _stdin_file = EnvRestore::set("JQ_STDIN_FILE", stdin_file.as_os_str());

    let output = JqTool
        .execute(
            "jq",
            serde_json::json!({
                "filter": ".name",
                "input": "{\"name\":\"Ada\"}",
                "raw_output": true,
                "compact_output": true,
                "sort_keys": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let args = fs::read_to_string(args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec!["-r", "-c", "-S", ".name"]
    );
    assert_eq!(
        fs::read_to_string(stdin_file).unwrap(),
        "{\"name\":\"Ada\"}"
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "jq");
    assert_eq!(output["filter"], ".name");
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["stdout"], "Ada\n");
    assert_eq!(output["stderr"], Value::Null);
    assert_eq!(output["truncated"], false);
}

#[tokio::test]
async fn file_input_resolves_relative_path_against_cwd_and_uses_stdin() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let stdin_file = bin_dir.path().join("stdin.json");
    fs::write(work_dir.path().join("data.json"), "{\"ok\":true}\n").unwrap();
    write_fake_jq(bin_dir.path(), "printf 'true\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args_file = EnvRestore::set("JQ_ARGS_FILE", args_file.as_os_str());
    let _stdin_file = EnvRestore::set("JQ_STDIN_FILE", stdin_file.as_os_str());

    let output = JqTool
        .execute(
            "jq",
            serde_json::json!({
                "filter": ".ok",
                "path": "data.json",
                "cwd": work_dir.path()
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    assert_eq!(fs::read_to_string(stdin_file).unwrap(), "{\"ok\":true}\n");
    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["stdout"], "true\n");
}

#[tokio::test]
async fn rejects_missing_or_multiple_input_sources_before_spawning_jq() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let stdin_file = dir.path().join("stdin.json");
    write_fake_jq(dir.path(), "printf 'should-not-run\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("JQ_ARGS_FILE", args_file.as_os_str());
    let _stdin_file = EnvRestore::set("JQ_STDIN_FILE", stdin_file.as_os_str());

    let missing = JqTool
        .execute("jq", serde_json::json!({ "filter": "." }), &make_ctx())
        .await
        .unwrap();
    let missing = json_output(&missing);
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["error"]["kind"], "invalid_request");

    let multiple = JqTool
        .execute(
            "jq",
            serde_json::json!({
                "filter": ".",
                "input": "{}",
                "path": "data.json"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let multiple = json_output(&multiple);
    assert_eq!(multiple["ok"], false);
    assert_eq!(multiple["error"]["kind"], "invalid_request");
    assert!(
        !args_file.exists(),
        "jq should not run when request validation fails"
    );
}

#[tokio::test]
async fn non_zero_jq_exit_is_structured() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let stdin_file = dir.path().join("stdin.json");
    write_fake_jq(
        dir.path(),
        "printf 'compile error\\n' >&2\nprintf 'partial\\n'\nexit 3\n",
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("JQ_ARGS_FILE", args_file.as_os_str());
    let _stdin_file = EnvRestore::set("JQ_STDIN_FILE", stdin_file.as_os_str());

    let output = JqTool
        .execute(
            "jq",
            serde_json::json!({ "filter": "bad(", "input": "{}" }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], 3);
    assert_eq!(output["stdout"], "partial\n");
    assert_eq!(output["stderr"], "compile error\n");
    assert_eq!(output["error"]["kind"], "jq_error");
}

#[tokio::test]
async fn missing_jq_returns_structured_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = JqTool
        .execute(
            "jq",
            serde_json::json!({ "filter": ".", "input": "{}" }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["error"]["kind"], "missing_jq");
    assert!(output["error"]["install"][0]
        .as_str()
        .unwrap()
        .contains("Install jq"));
}

#[tokio::test]
async fn output_truncation_preserves_valid_json() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    let stdin_file = dir.path().join("stdin.json");
    write_fake_jq(
        dir.path(),
        "i=0\nwhile [ \"$i\" -lt 2000 ]; do printf x; i=$((i + 1)); done\nprintf '\\n'\n",
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("JQ_ARGS_FILE", args_file.as_os_str());
    let _stdin_file = EnvRestore::set("JQ_STDIN_FILE", stdin_file.as_os_str());

    let rendered = JqTool
        .execute(
            "jq",
            serde_json::json!({
                "filter": ".",
                "input": "{}",
                "max_output_bytes": 700
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&rendered);

    assert_eq!(output["ok"], true);
    assert_eq!(output["truncated"], true);
    assert!(output["stdout"].as_str().unwrap().contains("[truncated]"));
    assert!(
        rendered.len() <= 700,
        "len={} output={rendered}",
        rendered.len()
    );
}
