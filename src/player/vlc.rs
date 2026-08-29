use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Stdio;
use tokio::process::{Child, Command};

pub async fn launch(vlc_path: Option<&Path>, url: &str, verbose: bool) -> Result<Child> {
    #[cfg(target_os = "macos")]
    let default_path = Path::new("/Applications/VLC.app/Contents/MacOS/VLC");
    #[cfg(not(target_os = "macos"))]
    let default_path = Path::new("vlc");
    let executable = vlc_path.unwrap_or(default_path);

    let mut command = Command::new(executable);
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
        if vlc_path.is_some() {
            format!("unable to launch VLC at {}", executable.display())
        } else {
            "VLC was not found; install VLC or pass --vlc-path".to_string()
        }
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
    if let Some(path) = path
        && !path.is_file()
    {
        bail!("VLC executable does not exist: {}", path.display());
    }
    Ok(())
}
