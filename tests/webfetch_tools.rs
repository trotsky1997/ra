//! Integration tests for native webfetch-cli tool registration and execution
//! boundaries. These use a fake `npm` binary so the tests never hit the
//! network or install the GitHub package.

use ra::{
    tools::{Tool, WebfetchCrawlTool, WebfetchFetchTool},
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
    let (events, _) = tokio::sync::broadcast::channel(16);
    ToolCtx::local(events)
}

fn make_ctx_with_events() -> (ToolCtx, tokio::sync::broadcast::Receiver<Event>) {
    let (events, rx) = tokio::sync::broadcast::channel(16);
    (ToolCtx::local(events), rx)
}

fn json_output(output: &str) -> Value {
    serde_json::from_str(output).unwrap_or_else(|err| panic!("invalid json: {err}: {output}"))
}

fn write_fake_npm(dir: &Path, body: &str) {
    let path = dir.join("npm");
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$WEBFETCH_ARGS_FILE\"\n{body}\n"),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn default_catalog_contains_webfetch_tools_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();

    assert!(
        names.contains(&"webfetch_fetch".to_string()),
        "missing webfetch_fetch: {names:?}"
    );
    assert!(
        names.contains(&"webfetch_crawl".to_string()),
        "missing webfetch_crawl: {names:?}"
    );

    let filtered = ra::default_builtins(&["webfetch_fetch".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["webfetch_fetch"]);
}

#[tokio::test]
async fn fetch_returns_structured_missing_npm_guidance() {
    let _guard = env_lock().lock().await;
    let empty_path = tempfile::tempdir().unwrap();
    let _path = EnvRestore::set("PATH", empty_path.path().as_os_str());

    let output = WebfetchFetchTool
        .execute(
            "wf",
            serde_json::json!({ "url": "https://example.com" }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let output = json_output(&output);
    assert_eq!(output["ok"], false);
    assert_eq!(output["error"]["kind"], "missing_npm");
    assert!(output["error"]["install"][0]
        .as_str()
        .unwrap()
        .contains("Node.js"));
}

#[tokio::test]
async fn fetch_executes_fake_npm_with_expected_argv_and_bounds_output() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_npm(
        dir.path(),
        "printf '{\"text\":\"%02000d\"}\\n' 0\nprintf 'progress on stderr\\n' >&2\n",
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("WEBFETCH_ARGS_FILE", args_file.as_os_str());

    let output = WebfetchFetchTool
        .execute(
            "wf",
            serde_json::json!({
                "url": "https://example.com/docs",
                "output": "all",
                "timeout_ms": 60000,
                "cwd": "scratch",
                "raw_only": true,
                "max_output_bytes": 700
            }),
            &make_ctx(),
        )
        .await
        .unwrap();

    let args = fs::read_to_string(args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "exec",
            "--yes",
            "--package",
            "github:trotsky1997/webfetch-cli",
            "--",
            "webfetch-cli",
            "fetch",
            "https://example.com/docs",
            "--output",
            "all",
            "--timeout",
            "60000",
            "--cwd",
            "scratch",
            "--raw-only",
            "--json",
        ]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "webfetch_fetch");
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["truncated"], true);
}

#[tokio::test]
async fn crawl_executes_fake_npm_with_expected_argv() {
    let _guard = env_lock().lock().await;
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args.txt");
    write_fake_npm(
        dir.path(),
        "printf '{\"details\":{\"crawl\":{\"fetchedPages\":[]}}}\\n'\nprintf 'crawl progress\\n' >&2\n",
    );
    let _path = EnvRestore::set("PATH", dir.path().as_os_str());
    let _args_file = EnvRestore::set("WEBFETCH_ARGS_FILE", args_file.as_os_str());
    let (ctx, mut rx) = make_ctx_with_events();

    let output = WebfetchCrawlTool
        .execute(
            "wc",
            serde_json::json!({
                "url": "https://docs.example.com/start",
                "parent_domain": "example.com",
                "max_hops": 1,
                "max_pages": 10,
                "max_links": 20,
                "concurrency": 2,
                "allow_query": true,
                "output": "all",
                "timeout_ms": 45000,
                "cwd": "/tmp/webfetch",
                "raw_only": true,
                "verbose": true
            }),
            &ctx,
        )
        .await
        .unwrap();

    let args = fs::read_to_string(args_file).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "exec",
            "--yes",
            "--package",
            "github:trotsky1997/webfetch-cli",
            "--",
            "webfetch-cli",
            "crawl",
            "https://docs.example.com/start",
            "--parent-domain",
            "example.com",
            "--max-hops",
            "1",
            "--max-pages",
            "10",
            "--max-links",
            "20",
            "--concurrency",
            "2",
            "--allow-query",
            "--output",
            "all",
            "--timeout",
            "45000",
            "--cwd",
            "/tmp/webfetch",
            "--raw-only",
            "--verbose",
            "--json",
        ]
    );

    let output = json_output(&output);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], "webfetch_crawl");

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ToolCallUpdate { chunk, .. } if chunk.contains("crawl progress")
    )));
}
