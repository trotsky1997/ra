//! Integration tests for the native mergiraf tool. These use a fake
//! `mergiraf` binary so the tests verify Ra's argv/envelope behavior without
//! depending on the host mergiraf installation.

use ra::{
    tools::{MergirafTool, Tool},
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

fn write_fake_mergiraf(dir: &Path, body: &str) {
    let path = dir.join("mergiraf");
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$MERGIRAF_ARGS_FILE\"\n{body}\n"),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn default_catalog_contains_mergiraf_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    assert!(
        names.contains(&"mergiraf".to_string()),
        "missing mergiraf: {names:?}"
    );

    let filtered = ra::default_builtins(&["mergiraf".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["mergiraf"]);
}

#[tokio::test]
async fn merge_executes_fake_mergiraf_with_expected_argv_and_envelope() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_mergiraf(dir.path(), "printf 'merged\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("MERGIRAF_ARGS_FILE", args_file.as_os_str());

    let output = MergirafTool
        .execute(
            "mergiraf",
            serde_json::json!({
                "action": "merge",
                "base": "base file.rs",
                "ours": "ours;still-one-arg.rs",
                "theirs": "theirs.rs",
                "language": "rust",
                "compact": true,
                "allow_parse_errors": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "merge",
            "base file.rs",
            "ours;still-one-arg.rs",
            "theirs.rs",
            "--language",
            "rust",
            "--compact",
            "--allow-parse-errors"
        ]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "mergiraf");
    assert_eq!(output["action"], "merge");
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["stdout"], "merged\n");
    assert_eq!(output["stderr"], Value::Null);
    assert_eq!(output["truncated"], false);
}

#[tokio::test]
async fn solve_passes_file_path_as_single_argv_and_reports_non_zero_exit() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_mergiraf(
        dir.path(),
        "printf 'conflicts remain\\n' >&2\nprintf 'partial\\n'\nexit 2\n",
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("MERGIRAF_ARGS_FILE", args_file.as_os_str());

    let output = MergirafTool
        .execute(
            "mergiraf",
            serde_json::json!({
                "action": "solve",
                "file": "path with spaces/conflicted.rs"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["solve", "path with spaces/conflicted.rs"]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["action"], "solve");
    assert_eq!(output["exit_code"], 2);
    assert_eq!(output["stdout"], "partial\n");
    assert_eq!(output["stderr"], "conflicts remain\n");
    assert_eq!(output["error"]["kind"], "mergiraf_error");
}

#[tokio::test]
async fn languages_adds_gitattributes_flag_and_returns_output() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_mergiraf(dir.path(), "printf '*.rs merge=mergiraf\\n'\n");
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("MERGIRAF_ARGS_FILE", args_file.as_os_str());

    let output = MergirafTool
        .execute(
            "mergiraf",
            serde_json::json!({ "action": "languages" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["languages", "--gitattributes"]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["action"], "languages");
    assert_eq!(output["stdout"], "*.rs merge=mergiraf\n");
}

#[tokio::test]
async fn missing_binary_returns_structured_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = MergirafTool
        .execute(
            "mergiraf",
            serde_json::json!({
                "action": "merge",
                "base": "base.rs",
                "ours": "ours.rs",
                "theirs": "theirs.rs"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["tool"], "mergiraf");
    assert_eq!(output["action"], "merge");
    assert_eq!(output["exit_code"], Value::Null);
    assert_eq!(output["error"]["kind"], "missing_mergiraf");
    assert!(output["error"]["install_hint"]
        .as_str()
        .unwrap()
        .contains("cargo install mergiraf"));
}

#[tokio::test]
async fn output_truncation_preserves_valid_json() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_mergiraf(
        dir.path(),
        "i=0\nwhile [ \"$i\" -lt 2000 ]; do printf x; i=$((i + 1)); done\nprintf '\\n'\n",
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("MERGIRAF_ARGS_FILE", args_file.as_os_str());

    let rendered = MergirafTool
        .execute(
            "mergiraf",
            serde_json::json!({
                "action": "languages",
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
