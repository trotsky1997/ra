//! Integration tests for the native openlsp tool registration.

use ra::config::OpenlspSection;
use ra::tools::default_builtins_with_cfg;

#[test]
fn lsp_absent_when_openlsp_disabled() {
    let cfg = OpenlspSection {
        enabled: false,
        binary: Some("true".to_string()), // `true` is always on PATH
        workspace_root: None,
        timeout: 30.0,
    };
    let names: Vec<String> = default_builtins_with_cfg(&[], &cfg)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        !names.contains(&"lsp".to_string()),
        "lsp should be absent when disabled; got: {names:?}"
    );
}

#[test]
fn lsp_present_when_binary_override_resolves() {
    // Use `true` (always on PATH) as a stand-in for openlsp-cli.
    let cfg = OpenlspSection {
        enabled: true,
        binary: Some("true".to_string()),
        workspace_root: None,
        timeout: 30.0,
    };
    let names: Vec<String> = default_builtins_with_cfg(&[], &cfg)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        names.contains(&"lsp".to_string()),
        "lsp should be present when binary resolves; got: {names:?}"
    );
}

#[test]
fn lsp_excluded_by_allowlist() {
    let cfg = OpenlspSection {
        enabled: true,
        binary: Some("true".to_string()),
        workspace_root: None,
        timeout: 30.0,
    };
    let allowlist = vec!["read".to_string(), "bash".to_string()];
    let names: Vec<String> = default_builtins_with_cfg(&allowlist, &cfg)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        !names.contains(&"lsp".to_string()),
        "lsp should be excluded by allowlist; got: {names:?}"
    );
}

#[test]
fn lsp_included_by_allowlist_when_binary_resolves() {
    let cfg = OpenlspSection {
        enabled: true,
        binary: Some("true".to_string()),
        workspace_root: None,
        timeout: 30.0,
    };
    let allowlist = vec!["read".to_string(), "lsp".to_string()];
    let names: Vec<String> = default_builtins_with_cfg(&allowlist, &cfg)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        names.contains(&"lsp".to_string()),
        "lsp should be included when in allowlist and binary resolves; got: {names:?}"
    );
}
