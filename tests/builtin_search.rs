//! Integration tests for the pure-Rust grep/find/ls built-in tools.
//!
//! These tests verify that the tools work without any external binaries
//! (rg, fd, eza) by exercising them directly against a temp directory.

use ra::{
    tool_ctx::ToolCtx,
    tools::{FindTool, GrepTool, LsTool, Tool},
};
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn make_ctx() -> ToolCtx {
    let (tx, _) = tokio::sync::broadcast::channel(16);
    ToolCtx::local(tx)
}

fn setup_tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("main.rs"), "fn main() {\n    println!(\"hello world\");\n}\n").unwrap();
    fs::write(root.join("lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();
    fs::write(root.join("README.md"), "# Test\nThis is a test project.\n").unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/util.rs"), "pub fn helper() {}\n").unwrap();
    fs::write(root.join("src/config.rs"), "pub struct Config { pub debug: bool }\n").unwrap();
    // Hidden file — should be excluded by default
    fs::write(root.join(".hidden"), "secret").unwrap();

    dir
}

// ---------- GrepTool tests ---------------------------------------------------

#[tokio::test]
async fn grep_finds_pattern() {
    let dir = setup_tree();
    let tool = GrepTool;
    let result = tool
        .execute(
            "t1",
            json!({ "pattern": "hello", "path": dir.path().to_str().unwrap() }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("hello"), "expected 'hello' in output, got: {result}");
    assert!(result.contains("main.rs"), "expected main.rs in output");
}

#[tokio::test]
async fn grep_no_match_returns_no_matches() {
    let dir = setup_tree();
    let tool = GrepTool;
    let result = tool
        .execute(
            "t2",
            json!({ "pattern": "XYZZY_NOTFOUND", "path": dir.path().to_str().unwrap() }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert_eq!(result.trim(), "(no matches)");
}

#[tokio::test]
async fn grep_files_with_matches() {
    let dir = setup_tree();
    let tool = GrepTool;
    let result = tool
        .execute(
            "t3",
            json!({
                "pattern": "pub",
                "path": dir.path().to_str().unwrap(),
                "files_with_matches": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    // lib.rs, src/util.rs, src/config.rs all contain "pub"
    assert!(result.contains("lib.rs"), "expected lib.rs: {result}");
}

#[tokio::test]
async fn grep_glob_filter() {
    let dir = setup_tree();
    let tool = GrepTool;
    let result = tool
        .execute(
            "t4",
            json!({
                "pattern": ".",
                "path": dir.path().to_str().unwrap(),
                "glob": "*.md",
                "files_with_matches": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("README.md"), "expected README.md: {result}");
    assert!(!result.contains("main.rs"), "should not contain main.rs: {result}");
}

#[tokio::test]
async fn grep_case_insensitive() {
    let dir = setup_tree();
    let tool = GrepTool;
    let result = tool
        .execute(
            "t5",
            json!({
                "pattern": "HELLO",
                "path": dir.path().to_str().unwrap(),
                "case_insensitive": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("hello"), "expected case-insensitive match: {result}");
}

#[tokio::test]
async fn grep_fixed_string() {
    let dir = setup_tree();
    let tool = GrepTool;
    // "i32" is a literal string, not a regex special
    let result = tool
        .execute(
            "t6",
            json!({
                "pattern": "i32",
                "path": dir.path().to_str().unwrap(),
                "fixed_string": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("lib.rs"), "expected lib.rs: {result}");
}

// ---------- FindTool tests ---------------------------------------------------

#[tokio::test]
async fn find_lists_all_files() {
    let dir = setup_tree();
    let tool = FindTool;
    let result = tool
        .execute(
            "f1",
            json!({ "path": dir.path().to_str().unwrap() }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("main.rs"), "expected main.rs: {result}");
    assert!(result.contains("lib.rs"), "expected lib.rs: {result}");
    assert!(result.contains("README.md"), "expected README.md: {result}");
}

#[tokio::test]
async fn find_regex_pattern() {
    let dir = setup_tree();
    let tool = FindTool;
    let result = tool
        .execute(
            "f2",
            json!({
                "pattern": "\\.rs$",
                "path": dir.path().to_str().unwrap()
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("main.rs"), "expected main.rs: {result}");
    assert!(!result.contains("README.md"), "should not contain README.md: {result}");
}

#[tokio::test]
async fn find_glob_pattern() {
    let dir = setup_tree();
    let tool = FindTool;
    let result = tool
        .execute(
            "f3",
            json!({
                "pattern": "*.rs",
                "path": dir.path().to_str().unwrap(),
                "glob": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("main.rs"), "expected main.rs: {result}");
    assert!(!result.contains("README.md"), "should not contain README.md: {result}");
}

#[tokio::test]
async fn find_extension_filter() {
    let dir = setup_tree();
    let tool = FindTool;
    let result = tool
        .execute(
            "f4",
            json!({
                "path": dir.path().to_str().unwrap(),
                "extension": "md"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("README.md"), "expected README.md: {result}");
    assert!(!result.contains("main.rs"), "should not contain main.rs: {result}");
}

#[tokio::test]
async fn find_type_dirs_only() {
    let dir = setup_tree();
    let tool = FindTool;
    let result = tool
        .execute(
            "f5",
            json!({
                "path": dir.path().to_str().unwrap(),
                "type": "d"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("src"), "expected src dir: {result}");
    assert!(!result.contains("main.rs"), "should not contain files: {result}");
}

#[tokio::test]
async fn find_max_results() {
    let dir = setup_tree();
    let tool = FindTool;
    let result = tool
        .execute(
            "f6",
            json!({
                "path": dir.path().to_str().unwrap(),
                "max_results": 2
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = result.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() <= 2, "expected at most 2 results, got {}: {result}", lines.len());
}

// ---------- LsTool tests -----------------------------------------------------

#[tokio::test]
async fn ls_lists_directory() {
    let dir = setup_tree();
    let tool = LsTool;
    let result = tool
        .execute(
            "l1",
            json!({ "path": dir.path().to_str().unwrap() }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains("main.rs"), "expected main.rs: {result}");
    assert!(result.contains("src/"), "expected src/ dir: {result}");
    // Hidden file should not appear by default
    assert!(!result.contains(".hidden"), "should not show hidden: {result}");
}

#[tokio::test]
async fn ls_all_shows_hidden() {
    let dir = setup_tree();
    let tool = LsTool;
    let result = tool
        .execute(
            "l2",
            json!({ "path": dir.path().to_str().unwrap(), "all": true }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(result.contains(".hidden"), "expected .hidden: {result}");
}

#[tokio::test]
async fn ls_long_format() {
    let dir = setup_tree();
    let tool = LsTool;
    let result = tool
        .execute(
            "l3",
            json!({ "path": dir.path().to_str().unwrap(), "long": true }),
            &make_ctx(),
        )
        .await
        .unwrap();
    // Long format includes size info
    assert!(result.contains("main.rs"), "expected main.rs: {result}");
    // Should contain a size indicator
    assert!(result.contains('B') || result.contains('K'), "expected size in output: {result}");
}

#[tokio::test]
async fn ls_tree_format() {
    let dir = setup_tree();
    let tool = LsTool;
    let result = tool
        .execute(
            "l4",
            json!({ "path": dir.path().to_str().unwrap(), "tree": true }),
            &make_ctx(),
        )
        .await
        .unwrap();
    // Tree format uses box-drawing characters
    assert!(result.contains("──"), "expected tree chars: {result}");
    assert!(result.contains("src/"), "expected src/ in tree: {result}");
    assert!(result.contains("util.rs"), "expected util.rs in tree: {result}");
}
