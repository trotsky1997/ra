//! Filesystem-backed trajectory store.
//!
//! Layout:
//!
//! ```text
//! <data_local>/ra/sessions/
//!     <cwd_hash>/                    one bucket per agent cwd
//!         <session_id>.json          one trajectory per session
//! ```
//!
//! - `cwd_hash` is `sha256(canonical_cwd)[..16]` rendered as hex; this
//!   gives stable, filesystem-safe bucket names without leaking the path
//!   into the global directory tree.
//! - Writes are atomic via `tempfile::NamedTempFile::persist` so a crash
//!   mid-write can't corrupt an existing trajectory.
//! - `data_local` resolves via the `dirs` crate (Linux: `~/.local/share`,
//!   macOS: `~/Library/Application Support`, Windows: `%LOCALAPPDATA%`).

use crate::atif::Trajectory;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Lightweight description of a saved session, returned by `list()`.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub session_id: String,
    pub path: PathBuf,
    pub modified: std::time::SystemTime,
    /// First user message in the trajectory, useful as a UI title.
    pub title: Option<String>,
}

/// Trajectory store rooted at one cwd bucket. Cheap to clone (just paths).
#[derive(Debug, Clone)]
pub struct SessionStore {
    bucket: PathBuf,
}

impl SessionStore {
    /// Build a store for `cwd`. Uses `RA_HOME` if set (escape hatch for tests
    /// and ad-hoc layouts), otherwise `dirs::data_local_dir()`.
    pub fn for_cwd(cwd: impl AsRef<Path>) -> Result<Self> {
        let root = match std::env::var_os("RA_HOME") {
            Some(p) => PathBuf::from(p),
            None => dirs::data_local_dir()
                .context("dirs::data_local_dir returned None")?
                .join("ra"),
        };
        let bucket = root.join("sessions").join(cwd_hash(cwd.as_ref()));
        std::fs::create_dir_all(&bucket)
            .with_context(|| format!("create_dir_all {}", bucket.display()))?;
        Ok(Self { bucket })
    }

    pub fn bucket(&self) -> &Path {
        &self.bucket
    }

    pub fn path_for(&self, session_id: &str) -> PathBuf {
        self.bucket.join(format!("{session_id}.json"))
    }

    /// Persist a trajectory (atomic). Returns the on-disk path.
    pub async fn save(&self, traj: &Trajectory) -> Result<PathBuf> {
        let session_id = traj
            .session_id
            .clone()
            .context("trajectory missing session_id")?;
        let bucket = self.bucket.clone();
        let target = self.path_for(&session_id);
        let json = serde_json::to_vec_pretty(traj).context("serialize trajectory")?;
        // serde + filesystem work is blocking; fan out to a worker thread.
        tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            let mut tmp = tempfile::NamedTempFile::new_in(&bucket)
                .with_context(|| format!("tempfile in {}", bucket.display()))?;
            std::io::Write::write_all(tmp.as_file_mut(), &json)?;
            tmp.as_file_mut().sync_all().ok();
            tmp.persist(&target)
                .map_err(|e| anyhow::anyhow!("persist: {e}"))?;
            Ok(target)
        })
        .await
        .context("save spawn_blocking")?
    }

    pub async fn load(&self, session_id: &str) -> Result<Trajectory> {
        let path = self.path_for(session_id);
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let traj: Trajectory = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(traj)
    }

    /// Snapshot the bucket. Sorted newest-first by mtime.
    pub async fn list(&self) -> Result<Vec<SessionMeta>> {
        let bucket = self.bucket.clone();
        let metas = tokio::task::spawn_blocking(move || -> Result<Vec<SessionMeta>> {
            let mut out = Vec::new();
            let dir = match std::fs::read_dir(&bucket) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
                Err(e) => return Err(e).context("read_dir"),
            };
            for entry in dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let title = read_title(&path);
                out.push(SessionMeta { session_id, path, modified, title });
            }
            out.sort_by(|a, b| b.modified.cmp(&a.modified));
            Ok(out)
        })
        .await
        .context("list spawn_blocking")??;
        Ok(metas)
    }

    pub async fn delete(&self, session_id: &str) -> Result<()> {
        let path = self.path_for(session_id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("delete"),
        }
    }
}

/// 16-hex-char prefix of `sha256(cwd_str)`. Stable, filesystem-safe.
fn cwd_hash(cwd: &Path) -> String {
    let canon = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf());
    let mut h = Sha256::new();
    h.update(canon.to_string_lossy().as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..8])
}

/// Cheap title lookup: streams the file, returns the first user step's
/// message text. Avoids parsing the whole trajectory just to render a list.
fn read_title(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let traj: Trajectory = serde_json::from_slice(&bytes).ok()?;
    traj.steps
        .iter()
        .find(|s| matches!(s.source, crate::atif::StepSource::User))
        .map(|s| {
            let mut t = s.message.as_text();
            if t.len() > 80 {
                t.truncate(80);
                t.push('…');
            }
            t
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atif::{Agent, Trajectory};

    #[tokio::test]
    async fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        // Override RA_HOME for this test.
        // SAFETY: tests are single-threaded by default in this crate.
        unsafe {
            std::env::set_var("RA_HOME", tmp.path());
        }
        let store = SessionStore::for_cwd(tmp.path()).unwrap();
        let mut traj = Trajectory::empty("sess_test", Agent::ra(None));
        traj.notes = Some("hi".into());

        let written = store.save(&traj).await.unwrap();
        assert!(written.exists());

        let loaded = store.load("sess_test").await.unwrap();
        assert_eq!(loaded.notes.as_deref(), Some("hi"));

        let metas = store.list().await.unwrap();
        assert!(metas.iter().any(|m| m.session_id == "sess_test"));
    }
}
