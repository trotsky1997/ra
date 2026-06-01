//! Integration tests for native task/workflow CLI tools. These use fake
//! binaries so the tests verify argv, cwd, and envelope behavior without
//! requiring mise, just, or wrkflw on the host.

use ra::{
    tools::{JustTool, MiseTool, Tool, WrkflwTool},
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

fn write_fake_binary(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TASK_ARGS_FILE\"\npwd > \"$TASK_CWD_FILE\"\n{body}\n"
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn default_catalog_contains_task_workflow_tools_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    for name in ["mise", "just", "wrkflw"] {
        assert!(
            names.contains(&name.to_string()),
            "missing {name}: {names:?}"
        );
    }

    for name in ["mise", "just", "wrkflw"] {
        let filtered = ra::default_builtins(&[name.to_string()])
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(filtered, vec![name]);
    }
}

#[tokio::test]
async fn mise_executes_fake_binary_with_expected_argv_and_cwd() {
    assert_tool_preserves_argv_and_cwd(
        "mise",
        &MiseTool,
        serde_json::json!({ "args": ["run", "test"], "cwd": "." }),
        vec!["run", "test"],
    )
    .await;
}

#[tokio::test]
async fn just_executes_fake_binary_with_expected_argv_and_cwd() {
    assert_tool_preserves_argv_and_cwd(
        "just",
        &JustTool,
        serde_json::json!({ "args": ["test"], "cwd": "." }),
        vec!["test"],
    )
    .await;
}

#[tokio::test]
async fn wrkflw_executes_fake_binary_with_expected_argv_and_cwd() {
    assert_tool_preserves_argv_and_cwd(
        "wrkflw",
        &WrkflwTool,
        serde_json::json!({ "args": ["validate", ".github/workflows/ci.yml"], "cwd": "." }),
        vec!["validate", ".github/workflows/ci.yml"],
    )
    .await;
}

async fn assert_tool_preserves_argv_and_cwd(
    binary: &str,
    tool: &dyn Tool,
    input: serde_json::Value,
    expected_args: Vec<&str>,
) {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_binary(bin_dir.path(), binary, "printf 'tests passed\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args_file = EnvRestore::set("TASK_ARGS_FILE", args_file.as_os_str());
    let _cwd_file = EnvRestore::set("TASK_CWD_FILE", cwd_file.as_os_str());

    let mut ctx = make_ctx();
    ctx.cwd = work_dir.path().to_path_buf();

    let output = tool.execute(binary, input, &ctx).await.unwrap();

    assert_eq!(
        fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected_args
    );
    assert_eq!(
        fs::read_to_string(cwd_file).unwrap().trim(),
        work_dir.path().to_str().unwrap()
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], binary);
    assert_eq!(output["command"]["program"], binary);
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["stdout"], "tests passed\n");
    assert_eq!(output["stderr"], Value::Null);
    assert_eq!(output["truncated"], false);
}

#[tokio::test]
async fn non_zero_exit_is_structured() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_binary(
        bin_dir.path(),
        "just",
        "printf 'compile failed\\n' >&2\nprintf 'partial\\n'\nexit 2\n",
    );
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args_file = EnvRestore::set("TASK_ARGS_FILE", args_file.as_os_str());
    let _cwd_file = EnvRestore::set("TASK_CWD_FILE", cwd_file.as_os_str());

    let output = JustTool
        .execute("just", serde_json::json!({ "args": ["test"] }), &make_ctx())
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], 2);
    assert_eq!(output["stdout"], "partial\n");
    assert_eq!(output["stderr"], "compile failed\n");
    assert_eq!(output["error"]["kind"], "command_failed");
}

#[tokio::test]
async fn missing_binary_returns_structured_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = WrkflwTool
        .execute(
            "wrkflw",
            serde_json::json!({ "args": ["validate"] }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["tool"], "wrkflw");
    assert_eq!(output["exit_code"], Value::Null);
    assert_eq!(output["error"]["kind"], "missing_wrkflw");
    assert!(output["error"]["install"][0]
        .as_str()
        .unwrap()
        .contains("wrkflw"));
}

#[tokio::test]
async fn timeout_returns_structured_error() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_binary(bin_dir.path(), "mise", "/bin/sleep 2\nprintf 'late\\n'\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args_file = EnvRestore::set("TASK_ARGS_FILE", args_file.as_os_str());
    let _cwd_file = EnvRestore::set("TASK_CWD_FILE", cwd_file.as_os_str());

    let output = MiseTool
        .execute(
            "mise",
            serde_json::json!({ "args": ["run", "test"], "timeout_ms": 25 }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["ok"], false);
    assert_eq!(output["exit_code"], Value::Null);
    assert_eq!(output["error"]["kind"], "timeout");
}

#[tokio::test]
async fn output_budget_preserves_valid_json_and_marks_truncated() {
    let _guard = env_lock().lock().await;
    let bin_dir = tempfile::tempdir().unwrap();
    let args_file = bin_dir.path().join("args.txt");
    let cwd_file = bin_dir.path().join("cwd.txt");
    write_fake_binary(bin_dir.path(), "just", "printf '%02000d\\n' 0\n");
    let _path = EnvRestore::set("PATH", bin_dir.path().as_os_str());
    let _args_file = EnvRestore::set("TASK_ARGS_FILE", args_file.as_os_str());
    let _cwd_file = EnvRestore::set("TASK_CWD_FILE", cwd_file.as_os_str());

    let output = JustTool
        .execute(
            "just",
            serde_json::json!({ "args": ["test"], "max_output_bytes": 600 }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);

    assert_eq!(output["truncated"], true);
    assert!(output["stdout"].as_str().unwrap().contains("[truncated]"));
}
