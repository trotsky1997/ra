//! Integration tests for the native sd tool. These use a fake `sd` binary so
//! the tests verify Ra's argv/cwd/envelope behavior without depending on the
//! host sd installation.

use ra::{
    tools::{SdTool, Tool},
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

fn write_fake_sd(dir: &Path, body: &str) {
    let path = dir.join("sd");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$SD_ARGS_FILE\"\npwd > \"$SD_CWD_FILE\"\n{body}\n"
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[tokio::test]
async fn default_catalog_contains_sd_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    assert!(names.contains(&"sd".to_string()), "missing sd: {names:?}");

    let filtered = ra::default_builtins(&["sd".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["sd"]);
}

#[tokio::test]
async fn basic_regex_replacement_preserves_argv_and_cwd() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    let source = work_dir.path().join("data.txt");
    fs::write(&source, "alpha beta\n").unwrap();
    write_fake_sd(
        bin_dir.path(),
        "if [ \"$1\" = \"--\" ] && [ \"$2\" = \"alpha\" ] && [ \"$3\" = \"gamma\" ]; then printf 'gamma beta\\n' > \"$4\"; fi\nprintf 'rewrote\\n'\n",
    );
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    let mut ctx = make_ctx();
    ctx.cwd = work_dir.path().to_path_buf();
    let output = SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "alpha",
                "replace": "gamma",
                "paths": ["data.txt"]
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
        vec!["--", "alpha", "gamma", "data.txt"]
    );
    assert_eq!(
        fs::read_to_string(cwd_file).unwrap().trim(),
        work_dir.path().to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "gamma beta\n");

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "sd");
    assert_eq!(output["command"]["program"], "sd");
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["stdout"], "rewrote\n");
    assert_eq!(output["stderr"], Value::Null);
    assert_eq!(output["truncated"], false);
}

#[tokio::test]
async fn capture_group_replacement_is_passed_verbatim() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_sd(bin_dir.path(), "printf 'ok\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "(foo)-(bar)",
                "replace": "$2-$1",
                "paths": ["data.txt"]
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
        vec!["--", "(foo)-(bar)", "$2-$1", "data.txt"]
    );
}

#[tokio::test]
async fn string_mode_and_extra_args_are_passed_before_positionals() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_sd(bin_dir.path(), "printf 'ok\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "a.b",
                "replace": "x",
                "paths": ["data.txt"],
                "string_mode": true,
                "extra_args": ["--flags", "i"]
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
            "--fixed-strings",
            "--flags",
            "i",
            "--",
            "a.b",
            "x",
            "data.txt"
        ]
    );
}

#[tokio::test]
async fn leading_dash_find_and_replace_are_not_parsed_as_flags() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_sd(bin_dir.path(), "printf 'ok\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "-old",
                "replace": "-new",
                "paths": ["data.txt"]
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
        vec!["--", "-old", "-new", "data.txt"]
    );
}

#[tokio::test]
async fn empty_paths_are_rejected_before_spawning_sd() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_sd(bin_dir.path(), "printf 'should-not-run\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    let output = SdTool
        .execute(
            "sd",
            serde_json::json!({ "find": "a", "replace": "b", "paths": [] }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["error"]["kind"], "invalid_request");
    assert!(
        !args_file.exists(),
        "sd should not run when request validation fails"
    );
}

#[tokio::test]
async fn missing_sd_returns_structured_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "a",
                "replace": "b",
                "paths": ["data.txt"]
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], Value::Null);
    assert_eq!(output["error"]["kind"], "missing_sd");
    assert!(output["error"]["install"][0]
        .as_str()
        .unwrap()
        .contains("cargo install sd"));
}

#[tokio::test]
async fn non_zero_sd_exit_is_structured() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_sd(
        bin_dir.path(),
        "printf 'partial\\n'\nprintf 'bad pattern\\n' >&2\nexit 2\n",
    );
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    let output = SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "[",
                "replace": "b",
                "paths": ["data.txt"]
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], 2);
    assert_eq!(output["stdout"], "partial\n");
    assert_eq!(output["stderr"], "bad pattern\n");
    assert_eq!(output["error"]["kind"], "command_failed");
}

#[tokio::test]
async fn output_truncation_preserves_valid_json() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_sd(bin_dir.path(), "printf '%02000d\\n' 0\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args = EnvRestore::set("SD_ARGS_FILE", args_file.as_os_str());
    let _cwd = EnvRestore::set("SD_CWD_FILE", cwd_file.as_os_str());

    let rendered = SdTool
        .execute(
            "sd",
            serde_json::json!({
                "find": "a",
                "replace": "b",
                "paths": ["data.txt"],
                "max_output_bytes": 600
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&rendered);

    assert_eq!(output["truncated"], true);
    assert!(output["stdout"].as_str().unwrap().contains("[truncated]"));
}
