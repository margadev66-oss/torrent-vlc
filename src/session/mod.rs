pub mod recovery;

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use recovery::{SessionMetadata, now_unix_seconds};
use serde_json::to_vec_pretty;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use tokio::task::spawn_blocking;
use uuid::Uuid;

pub struct SessionGuard {
    path: Option<PathBuf>,
    lock: Option<File>,
}

impl SessionGuard {
    pub fn create(sessions_root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&sessions_root).with_context(|| {
            format!(
                "unable to create session directory {}",
                sessions_root.display()
            )
        })?;
        let session_id = Uuid::new_v4().simple().to_string();
        let path = sessions_root.join(format!("session-{session_id}"));
        fs::create_dir(&path)
            .with_context(|| format!("unable to create temporary session {}", path.display()))?;
        let cleanup_path = path.clone();
        let result = (|| {
            fs::create_dir(path.join("metadata"))?;
            fs::create_dir(path.join("data"))?;

            let lock_path = path.join("session.lock");
            let lock = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&lock_path)
                .context("unable to create session ownership lock")?;
            lock.try_lock_exclusive()
                .context("unable to lock new session")?;

            let metadata = SessionMetadata {
                session_id,
                pid: std::process::id(),
                created_at: now_unix_seconds(),
                temporary: true,
            };
            write_metadata(&path, &metadata)?;
            Ok(Self {
                path: Some(path.clone()),
                lock: Some(lock),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(cleanup_path);
        }
        result
    }

    pub fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("session path was already finalized")
    }

    pub fn data_path(&self) -> PathBuf {
        self.path().join("data")
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.path().join("metadata")
    }

    pub fn write_metadata_file(&self, key: &str, contents: &[u8]) -> Result<()> {
        if Path::new(key)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            bail!("metadata file name must be a single normal path component");
        }
        let path = self.metadata_path().join(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, contents)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub async fn cleanup(&mut self) -> Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        self.release_lock();
        let cleanup_path = path.clone();
        let result = spawn_blocking(move || fs::remove_dir_all(&cleanup_path))
            .await
            .context("session cleanup task failed")?;
        result.with_context(|| format!("unable to delete temporary session {}", path.display()))?;
        self.path.take();
        Ok(())
    }

    pub async fn preserve_data(&mut self, output_root: &Path, name: &str) -> Result<PathBuf> {
        let Some(path) = self.path.clone() else {
            bail!("session has already been finalized");
        };
        let output_root = output_root.to_path_buf();
        let name = name.to_string();
        self.release_lock();
        let result = spawn_blocking(move || preserve_data_sync(&path, &output_root, &name)).await;
        let destination = result.context("persistent output task failed")??;
        self.path.take();
        Ok(destination)
    }

    fn release_lock(&mut self) {
        if let Some(lock) = self.lock.take() {
            let _ = lock.unlock();
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        self.release_lock();
        // This is only a best-effort fallback for unwinding after an explicit
        // cleanup failed. The async shutdown path performs the same operation
        // without blocking the runtime.
        let _ = fs::remove_dir_all(path);
    }
}

fn write_metadata(path: &Path, metadata: &SessionMetadata) -> Result<()> {
    let bytes = to_vec_pretty(metadata)?;
    fs::write(path.join("session.json"), bytes)?;
    Ok(())
}

fn preserve_data_sync(path: &Path, output_root: &Path, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(output_root).with_context(|| {
        format!(
            "unable to create output directory {}",
            output_root.display()
        )
    })?;
    let data_path = path.join("data");
    if !data_path.is_dir() {
        bail!("session data directory is missing");
    }
    let safe_name = sanitize_name(name);
    let destination = unique_destination(output_root, &safe_name)?;
    fs::rename(&data_path, &destination).with_context(|| {
        format!(
            "unable to move preserved media from {} to {}",
            data_path.display(),
            destination.display()
        )
    })?;
    fs::remove_dir_all(path)
        .with_context(|| format!("unable to remove session metadata {}", path.display()))?;
    Ok(destination)
}

fn unique_destination(root: &Path, name: &str) -> Result<PathBuf> {
    let base = if name.is_empty() { "torrent" } else { name };
    for suffix in 0..10_000u32 {
        let candidate_name = if suffix == 0 {
            base.to_string()
        } else {
            format!("{base}-{suffix}")
        };
        let candidate = root.join(candidate_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("unable to find an unused persistent output name")
}

pub fn sanitize_name(value: &str) -> String {
    let mut result = String::with_capacity(value.len().min(80));
    for character in value.chars() {
        if result.len() >= 80 {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            result.push(character);
        } else {
            result.push('_');
        }
    }
    result.trim_matches(['.', '_', '-']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn cleanup_removes_session_tree() {
        let root = tempdir().unwrap();
        let mut guard = SessionGuard::create(root.path().join("sessions")).unwrap();
        let path = guard.path().to_path_buf();
        fs::write(guard.data_path().join("part.bin"), b"data").unwrap();
        guard.cleanup().await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn preserve_moves_data_and_removes_session_metadata() {
        let root = tempdir().unwrap();
        let output = root.path().join("Videos");
        let mut guard = SessionGuard::create(root.path().join("sessions")).unwrap();
        fs::write(guard.data_path().join("part.bin"), b"data").unwrap();
        let destination = guard.preserve_data(&output, "episode-abc").await.unwrap();
        assert_eq!(fs::read(destination.join("part.bin")).unwrap(), b"data");
        assert_eq!(
            fs::read_dir(root.path().join("sessions")).unwrap().count(),
            0
        );
    }
}
