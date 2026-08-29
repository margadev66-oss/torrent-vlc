use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub pid: u32,
    pub created_at: u64,
    pub temporary: bool,
}

pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub fn default_cache_root() -> PathBuf {
    directories::ProjectDirs::from("", "", "torrent-vlc")
        .map(|directories| directories.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("torrent-vlc"))
}

pub fn recover_stale_sessions(sessions_root: &Path) -> Result<Vec<PathBuf>> {
    if !sessions_root.exists() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(sessions_root).context("unable to inspect torrent-vlc sessions")? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let metadata_path = path.join("session.json");
        let metadata = match fs::read_to_string(&metadata_path)
            .ok()
            .and_then(|contents| serde_json::from_str::<SessionMetadata>(&contents).ok())
        {
            Some(metadata) => {
                if !metadata.temporary || process_is_alive(metadata.pid) {
                    continue;
                }
                Some(metadata)
            }
            None => {
                if !is_older_than(&metadata_path, Duration::from_secs(24 * 60 * 60)) {
                    continue;
                }
                None
            }
        };
        if metadata.is_none() {
            tracing::warn!(path = %path.display(), "recovering an old session with unreadable metadata");
        }

        let lock_path = path.join("session.lock");
        let lock = match OpenOptions::new().read(true).write(true).open(&lock_path) {
            Ok(lock) => lock,
            Err(_) => continue,
        };
        if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
            continue;
        }
        fs::remove_dir_all(&path)
            .with_context(|| format!("unable to remove stale session {}", path.display()))?;
        removed.push(path);
    }
    Ok(removed)
}

fn is_older_than(path: &Path, age: Duration) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|elapsed| elapsed >= age)
        .unwrap_or(false)
}

pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn leaves_sessions_owned_by_live_processes() {
        let root = tempdir().unwrap();
        let session = root.path().join("session-live");
        fs::create_dir(&session).unwrap();
        fs::write(
            session.join("session.json"),
            serde_json::to_vec(&SessionMetadata {
                session_id: "live".into(),
                pid: std::process::id(),
                created_at: now_unix_seconds(),
                temporary: true,
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(session.join("session.lock"), b"").unwrap();
        assert!(recover_stale_sessions(root.path()).unwrap().is_empty());
        assert!(session.exists());
    }

    #[test]
    fn removes_a_session_owned_by_a_dead_process() {
        let root = tempdir().unwrap();
        let session = root.path().join("session-dead");
        fs::create_dir(&session).unwrap();
        fs::write(
            session.join("session.json"),
            serde_json::to_vec(&SessionMetadata {
                session_id: "dead".into(),
                pid: i32::MAX as u32,
                created_at: now_unix_seconds(),
                temporary: true,
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(session.join("session.lock"), b"").unwrap();
        let removed = recover_stale_sessions(root.path()).unwrap();
        assert_eq!(removed, vec![session.clone()]);
        assert!(!session.exists());
    }
}
