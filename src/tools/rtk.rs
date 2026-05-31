//! Native RTK ([rust-token-killer](https://github.com/rtk-ai/rtk))
//! integration for BashTool.
//!
//! The contract with RTK is dead simple: given a raw shell command,
//! ```sh
//! REWRITTEN=$(rtk rewrite "$CMD") && CMD=$REWRITTEN
//! ```
//! `rtk rewrite` exits 0 + prints the optimized command on stdout when
//! it has a recipe, exits 1 + prints nothing when it doesn't (run the
//! original verbatim). Either way we never break the user.
//!
//! On most dev commands (git/cargo/pytest/docker/kubectl/...) RTK
//! collapses verbose output to a few lines, saving 60–90 % of the
//! bytes that would otherwise eat into the model's context window.
//!
//! Resolution rules (driven by `[rtk]` in ra.toml):
//!   - `mode = "auto"` (default): use RTK iff its binary is on PATH.
//!   - `mode = "on"` (or `mode = true`): require RTK; warn if missing.
//!   - `mode = "off"` (or `mode = false`): never call RTK.
//!   - `binary = "<path>"` overrides the PATH lookup.
//!   - `ultra_compact = true` passes `--ultra-compact` for tighter output.

use crate::config::{RtkMode, RtkSection};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

/// One per process. Cheap to clone (only an Arc<PathBuf> + a couple of bools).
#[derive(Debug, Clone, Default)]
pub struct RtkRewriter {
    bin: Option<Arc<PathBuf>>,
    ultra_compact: bool,
}

impl RtkRewriter {
    /// Resolve from config. Logs once at construction time so the user
    /// sees what mode they're actually getting.
    pub fn from_config(cfg: &RtkSection) -> Self {
        let bin = match cfg.mode {
            RtkMode::Off => None,
            RtkMode::Auto => locate(cfg.binary.as_deref()),
            RtkMode::On => match locate(cfg.binary.as_deref()) {
                Some(p) => Some(p),
                None => {
                    eprintln!(
                        "[ra::rtk] mode=on but rtk binary not found on PATH \
                         (and config.rtk.binary not set); BashTool will run \
                         commands verbatim"
                    );
                    None
                }
            },
        };
        if let Some(p) = &bin {
            eprintln!(
                "[ra::rtk] enabled — bash commands will be routed through {}{}",
                p.display(),
                if cfg.ultra_compact { " --ultra-compact" } else { "" }
            );
        }
        Self {
            bin: bin.map(Arc::new),
            ultra_compact: cfg.ultra_compact,
        }
    }

    /// `true` if RTK is wired up and rewrites will be attempted.
    pub fn is_active(&self) -> bool {
        self.bin.is_some()
    }

    /// Try to rewrite `cmd`. Returns `Some(rewritten)` only when RTK has a
    /// recipe (non-empty stdout). Otherwise — including any RTK error or
    /// absent binary — returns `None` so the caller falls back to the
    /// original command. Never bubbles up an error: a failed rewrite must
    /// not break BashTool.
    ///
    /// Note on RTK exit codes: `rtk rewrite` exits 1 with no output when
    /// no recipe matches, and exits with various non-zero codes (3 has
    /// been observed) AFTER successfully printing a rewritten command.
    /// The trustworthy signal is therefore "stdout non-empty", not
    /// status.success().
    pub async fn rewrite(&self, cmd: &str) -> Option<String> {
        let bin = self.bin.as_ref()?;
        let mut c = Command::new(bin.as_path());
        c.arg("rewrite");
        if self.ultra_compact {
            c.arg("--ultra-compact");
        }
        c.arg(cmd);
        c.stdin(std::process::Stdio::null());
        let out = match c.output().await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[ra::rtk] spawn failed: {e}");
                return None;
            }
        };
        let trimmed = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
}

/// Best-effort PATH lookup. Honours an explicit override.
fn locate(override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        let pb = PathBuf::from(shellexpand::tilde(p).into_owned());
        return if pb.exists() { Some(pb) } else { None };
    }
    which::which("rtk").ok()
}
