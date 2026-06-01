use ra::{
    tools::{ApplyPatchTool, FuzzyTool, GlobTool, GrepTool, LsTool, Tool},
    ToolCtx,
};
use serde_json::Value;
use std::{fs, path::Path};
use tempfile::TempDir;

fn make_ctx() -> ToolCtx {
    let (events, _) = tokio::sync::broadcast::channel(16);
    ToolCtx::local(events)
}

fn json_output(output: &str) -> Value {
    serde_json::from_str(output).unwrap_or_else(|err| panic!("invalid json: {err}: {output}"))
}

fn setup_tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join("src/nested")).unwrap();
    fs::create_dir_all(root.join("target/debug")).unwrap();
    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::create_dir_all(root.join(".git/objects")).unwrap();
    fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();
    fs::write(root.join("src/nested/lib.rs"), "pub fn helper() {}\n").unwrap();
    fs::write(root.join("src/main.py"), "print('hello')\n").unwrap();
    fs::write(root.join("ignored.log"), "hello ignored\n").unwrap();
    fs::write(
        root.join("target/debug/generated.rs"),
        "fn generated() {}\n",
    )
    .unwrap();
    fs::write(root.join("node_modules/pkg/index.js"), "console.log('x')\n").unwrap();
    fs::write(root.join(".hidden.rs"), "fn hidden() {}\n").unwrap();

    dir
}

#[tokio::test]
async fn default_catalog_contains_extended_tools_and_allowlist_is_exact() {
    let names = ra::default_builtins(&[])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    for name in ["grep", "glob", "ls", "fuzzy", "apply_patch"] {
        assert!(
            names.contains(&name.to_string()),
            "missing {name}: {names:?}"
        );
    }

    let filtered = ra::default_builtins(&["grep".to_string()])
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(filtered, vec!["grep"]);
}

#[tokio::test]
async fn grep_basename_glob_matches_nested_files_and_skips_non_matching_files() {
    let dir = setup_tree();
    let output = GrepTool
        .execute(
            "grep",
            serde_json::json!({
                "pattern": "hello",
                "path": dir.path(),
                "glob": "*.rs"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let matches = output["matches"].as_array().unwrap();

    assert_eq!(matches.len(), 1, "output={output}");
    assert_eq!(matches[0]["path"], "src/main.rs");
    assert!(!output.to_string().contains("main.py"));
}

#[tokio::test]
async fn grep_single_file_sets_truncated_when_limit_is_reached() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.txt");
    fs::write(&path, "needle one\nneedle two\n").unwrap();

    let output = GrepTool
        .execute(
            "grep",
            serde_json::json!({
                "pattern": "needle",
                "path": path,
                "max_matches": 1
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let matches = output["matches"].as_array().unwrap();

    assert_eq!(matches.len(), 1, "output={output}");
    assert_eq!(matches[0]["path"], "sample.txt");
    assert_eq!(output["truncated"], true);
}

#[tokio::test]
async fn grep_single_file_uses_relative_path_for_type_filtering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.rs");
    fs::write(&path, "fn sample() {}\n").unwrap();

    let output = GrepTool
        .execute(
            "grep",
            serde_json::json!({
                "pattern": "fn sample",
                "path": path,
                "type": "rust"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let matches = output["matches"].as_array().unwrap();

    assert_eq!(matches.len(), 1, "output={output}");
    assert_eq!(matches[0]["path"], "sample.rs");
}

#[tokio::test]
async fn grep_yaml_type_matches_yaml_and_yml_extensions() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("config.yaml"), "needle: yaml\n").unwrap();
    fs::write(dir.path().join("config.yml"), "needle: yml\n").unwrap();

    let output = GrepTool
        .execute(
            "grep",
            serde_json::json!({
                "pattern": "needle",
                "path": dir.path(),
                "type": "yaml"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let mut paths = output["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    paths.sort();

    assert_eq!(paths, vec!["config.yaml", "config.yml"]);
}

#[tokio::test]
async fn glob_skips_gitignored_and_build_directories_by_default() {
    let dir = setup_tree();
    let output = GlobTool
        .execute(
            "glob",
            serde_json::json!({
                "path": dir.path(),
                "pattern": "*.rs"
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let paths = output["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap().to_string())
        .collect::<Vec<_>>();

    assert!(paths.iter().any(|path| path == "src/main.rs"), "{paths:?}");
    assert!(
        paths.iter().any(|path| path == "src/nested/lib.rs"),
        "{paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(&dir.path().to_string_lossy().to_string())),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.contains("/target/")),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.contains("/.git/")),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.contains("/node_modules/")),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.ends_with(".hidden.rs")),
        "{paths:?}"
    );
}

#[tokio::test]
async fn glob_rejects_unknown_kind_filter() {
    let dir = setup_tree();
    let err = GlobTool
        .execute(
            "glob",
            serde_json::json!({
                "path": dir.path(),
                "pattern": "**/*",
                "type": "socket"
            }),
            &make_ctx(),
        )
        .await
        .expect_err("unknown kind should fail");

    assert!(err.to_string().contains("unknown kind filter"), "{err:#}");
}

#[tokio::test]
async fn ls_recursive_skips_default_noise_directories() {
    let dir = setup_tree();
    let output = LsTool
        .execute(
            "ls",
            serde_json::json!({
                "path": dir.path(),
                "recursive": true,
                "max_depth": 4
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let rendered = output.to_string();
    let entries = output["entries"].as_array().unwrap();
    let paths = entries
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();

    assert!(rendered.contains("src/main.rs"), "{rendered}");
    assert!(paths.iter().any(|path| path == "src/main.rs"), "{paths:?}");
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(&dir.path().to_string_lossy().to_string())),
        "{paths:?}"
    );
    assert!(
        !rendered.contains("target/debug/generated.rs"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("node_modules/pkg/index.js"),
        "{rendered}"
    );
    assert!(!rendered.contains(".git/objects"), "{rendered}");
}

#[tokio::test]
async fn fuzzy_returns_ranked_matches() {
    let output = FuzzyTool
        .execute(
            "fuzzy",
            serde_json::json!({
                "query": "main",
                "candidates": ["README.md", "src/main.rs", "src/model.rs"]
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    let output = json_output(&output);
    let matches = output["matches"].as_array().unwrap();

    assert_eq!(matches[0]["value"], "src/main.rs");
}

#[tokio::test]
async fn apply_patch_check_only_does_not_modify_file_then_apply_writes() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());
    let path = dir.path().join("sample.txt");
    fs::write(&path, "old\n").unwrap();
    let patch = "diff --git a/sample.txt b/sample.txt\n\
                 --- a/sample.txt\n\
                 +++ b/sample.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n";

    let check = ApplyPatchTool
        .execute(
            "patch-check",
            serde_json::json!({
                "cwd": dir.path(),
                "patch": patch,
                "check_only": true
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(check.contains("cleanly"), "{check}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "old\n");

    let applied = ApplyPatchTool
        .execute(
            "patch-apply",
            serde_json::json!({
                "cwd": dir.path(),
                "patch": patch
            }),
            &make_ctx(),
        )
        .await
        .unwrap();
    assert!(applied.contains("applied"), "{applied}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
}

#[tokio::test]
async fn apply_patch_rejects_missing_or_file_cwd_before_running_git() {
    let missing = tempfile::tempdir().unwrap().path().join("missing");
    let patch = "diff --git a/sample.txt b/sample.txt\n\
                 --- a/sample.txt\n\
                 +++ b/sample.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n";

    let err = ApplyPatchTool
        .execute(
            "patch-missing-cwd",
            serde_json::json!({
                "cwd": missing,
                "patch": patch,
                "check_only": true
            }),
            &make_ctx(),
        )
        .await
        .expect_err("missing cwd should fail before git");
    assert!(err.to_string().contains("cwd does not exist"), "{err:#}");

    let dir = tempfile::tempdir().unwrap();
    let file_cwd = dir.path().join("not-a-dir");
    fs::write(&file_cwd, "").unwrap();
    let err = ApplyPatchTool
        .execute(
            "patch-file-cwd",
            serde_json::json!({
                "cwd": file_cwd,
                "patch": patch,
                "check_only": true
            }),
            &make_ctx(),
        )
        .await
        .expect_err("file cwd should fail before git");
    assert!(
        err.to_string().contains("cwd must be a directory"),
        "{err:#}"
    );
}

#[tokio::test]
async fn apply_patch_failed_check_does_not_modify_file() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());
    let path = dir.path().join("sample.txt");
    fs::write(&path, "old\n").unwrap();
    let patch = "diff --git a/sample.txt b/sample.txt\n\
                 --- a/sample.txt\n\
                 +++ b/sample.txt\n\
                 @@ -1 +1 @@\n\
                 -missing\n\
                 +new\n";

    let err = ApplyPatchTool
        .execute(
            "patch-fail",
            serde_json::json!({
                "cwd": dir.path(),
                "patch": patch
            }),
            &make_ctx(),
        )
        .await
        .expect_err("invalid patch should fail");
    assert!(err.to_string().contains("patch check failed"), "{err:#}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "old\n");
}

fn init_git_repo(path: &Path) {
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(path)
        .status()
        .expect("spawn git init");
    assert!(status.success());
}
