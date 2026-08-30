use anyhow::{Context, Result, bail};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

pub async fn launch(vlc_path: Option<&Path>, url: &str, verbose: bool) -> Result<Child> {
    let executable = resolve_vlc_path(vlc_path)?;
    let mut command = Command::new(&executable);
    command
        // VLC can be configured to forward new URLs to an existing instance.
        // That makes the child returned by spawn() exit immediately, so the
        // session manager would incorrectly clean up while playback continues
        // in an unrelated VLC process. Always use a dedicated instance whose
        // lifetime belongs to this streaming session.
        .arg("--no-one-instance")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
    command.spawn().with_context(|| {
        format!(
            "unable to launch VLC at {}; install VLC or pass --vlc-path",
            executable.display()
        )
    })
}

pub async fn terminate(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_none() {
        child.start_kill().context("unable to stop VLC")?;
    }
    let _ = child.wait().await;
    Ok(())
}

pub fn validate_vlc_path(path: Option<&Path>) -> Result<()> {
    resolve_vlc_path(path).map(|_| ())
}

pub fn resolve_vlc_path(path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("VLC executable does not exist: {}", path.display());
    }

    for candidate in default_vlc_candidates() {
        let resolved = if candidate.is_absolute() {
            candidate.is_file().then_some(candidate)
        } else {
            find_on_path(&candidate)
        };
        if let Some(resolved) = resolved {
            return Ok(resolved);
        }
    }

    bail!("VLC was not found; install VLC or pass --vlc-path")
}

fn find_on_path(program: &Path) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|directory| {
        let candidate = directory.join(program);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(target_os = "macos")]
fn default_vlc_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/Applications/VLC.app/Contents/MacOS/VLC"),
        PathBuf::from("vlc"),
    ];
    if let Some(home) = env::var_os("HOME") {
        candidates.insert(
            1,
            PathBuf::from(home).join("Applications/VLC.app/Contents/MacOS/VLC"),
        );
    }
    candidates
}

#[cfg(windows)]
fn default_vlc_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("vlc.exe")];
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = env::var_os(variable) {
            candidates.push(PathBuf::from(root).join("VideoLAN/VLC/vlc.exe"));
        }
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local_app_data);
        candidates.push(root.join("Programs/VideoLAN/VLC/vlc.exe"));
        candidates.push(root.join("VideoLAN/VLC/vlc.exe"));
    }
    candidates
}

#[cfg(not(any(target_os = "macos", windows)))]
fn default_vlc_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("vlc"),
        PathBuf::from("/usr/bin/vlc"),
        PathBuf::from("/usr/local/bin/vlc"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn accepts_an_explicit_vlc_executable() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("vlc");
        fs::write(&path, b"executable fixture").unwrap();

        assert_eq!(resolve_vlc_path(Some(&path)).unwrap(), path);
    }

    #[test]
    fn rejects_a_missing_explicit_vlc_executable() {
        let path = PathBuf::from("missing-vlc-executable");
        let error = resolve_vlc_path(Some(&path)).unwrap_err().to_string();

        assert!(error.contains("VLC executable does not exist"));
    }
}
