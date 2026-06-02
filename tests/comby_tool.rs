//! Integration tests for the native comby tool. These use a fake `comby`
//! binary so the tests verify Ra's argv/cwd/envelope behavior without
//! depending on the host comby installation.

use ra::{
    tools::{CombyTool, Tool},
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

fn write_fake_comby(dir: &Path, body: &str) {
    let path = dir.join("comby");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$COMBY_ARGS_FILE\"\npwd > \"$COMBY_CWD_FILE\"\n{body}\n"
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[tokio::test]
async fn default_catalog_contains_comby_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    assert!(
        names.contains(&"comby".to_string()),
        "missing comby: {names:?}"
    );

    let filtered = ra::default_builtins(&["comby".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["comby"]);
}

#[tokio::test]
async fn rewrite_uses_in_place_and_preserves_filters() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    let source = work_dir.path().join("lib.rs");
    fs::write(&source, "fn old() {}\n").unwrap();
    write_fake_comby(
        bin_dir.path(),
        "for arg in \"$@\"; do if [ \"$arg\" = \"-in-place\" ]; then printf 'fn new() {}\\n' > lib.rs; fi; done\nprintf 'rewrote\\n'\n",
    );
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("COMBY_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("COMBY_CWD_FILE", cwd_file.as_os_str());

    let mut ctx = make_ctx();
    ctx.cwd = work_dir.path().to_path_buf();
    let output = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "rewrite",
                "match_template": "old(:[x])",
                "rewrite_template": "new(:[x])",
                "extensions": [".rs"],
                "directory": ".",
                "matcher": "rust",
                "include_files": ".*\\.rs",
                "exclude_files": "target",
                "extra_args": ["-jobs", "1"]
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "old(:[x])",
            "new(:[x])",
            ".rs",
            "-d",
            ".",
            "-matcher",
            "rust",
            "-include-files",
            ".*\\.rs",
            "-exclude-files",
            "target",
            "-in-place",
            "-jobs",
            "1"
        ]
    );
    assert_eq!(
        fs::read_to_string(cwd_file).unwrap().trim(),
        work_dir.path().to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "fn new() {}\n");

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "comby");
    assert_eq!(output["action"], "rewrite");
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["stdout"], "rewrote\n");
}

#[tokio::test]
async fn check_uses_match_only_without_rewrite_template() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_comby(bin_dir.path(), "printf 'match\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("COMBY_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("COMBY_CWD_FILE", cwd_file.as_os_str());

    let output = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "check",
                "match_template": "old(:[x])",
                "extensions": [".rs"]
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
        vec!["old(:[x])", "", ".rs", "-match-only"]
    );
    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["action"], "check");
}

#[tokio::test]
async fn diff_uses_diff_and_returns_output() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_comby(
        bin_dir.path(),
        "printf '%s\\n' '--- a/lib.rs' '+++ b/lib.rs'\n",
    );
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("COMBY_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("COMBY_CWD_FILE", cwd_file.as_os_str());

    let output = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "diff",
                "match_template": "old(:[x])",
                "rewrite_template": "new(:[x])"
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
        vec!["old(:[x])", "new(:[x])", "-diff"]
    );
    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["action"], "diff");
    assert!(output["stdout"].as_str().unwrap().contains("--- a/lib.rs"));
    assert!(output["stdout"].as_str().unwrap().contains("+++ b/lib.rs"));
}

#[tokio::test]
async fn rewrite_without_template_is_rejected_before_spawning_comby() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_comby(bin_dir.path(), "printf 'should-not-run\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("COMBY_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("COMBY_CWD_FILE", cwd_file.as_os_str());

    let output = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "rewrite",
                "match_template": "old(:[x])"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["action"], "rewrite");
    assert_eq!(output["error"]["kind"], "invalid_request");
    assert!(
        !args_file.exists(),
        "comby should not run when request validation fails"
    );
}

#[tokio::test]
async fn missing_comby_returns_structured_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "check",
                "match_template": "old(:[x])"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], Value::Null);
    assert_eq!(output["error"]["kind"], "missing_comby");
    assert!(output["error"]["install"][0]
        .as_str()
        .unwrap()
        .contains("comby"));
}

#[tokio::test]
async fn non_zero_comby_exit_is_structured() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_comby(
        bin_dir.path(),
        "printf 'partial\\n'\nprintf 'bad template\\n' >&2\nexit 3\n",
    );
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("COMBY_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("COMBY_CWD_FILE", cwd_file.as_os_str());

    let output = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "check",
                "match_template": "old(:[x])"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], 3);
    assert_eq!(output["stdout"], "partial\n");
    assert_eq!(output["stderr"], "bad template\n");
    assert_eq!(output["error"]["kind"], "command_failed");
}

#[tokio::test]
async fn output_truncation_preserves_valid_json() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_comby(bin_dir.path(), "printf '%02000d\\n' 0\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("COMBY_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("COMBY_CWD_FILE", cwd_file.as_os_str());

    let rendered = CombyTool
        .execute(
            "comby",
            serde_json::json!({
                "action": "check",
                "match_template": "old(:[x])",
                "max_output_bytes": 700
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&rendered);

    assert_eq!(output["truncated"], true);
    assert!(output["stdout"].as_str().unwrap().contains("[truncated]"));
}
