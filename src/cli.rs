use crate::size::{parse_byte_size, parse_duration};
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(
    name = "torrent-vlc",
    version,
    about = "Stream an authorized BitTorrent media file into VLC using temporary storage"
)]
pub struct Cli {
    /// A magnet URI or a local .torrent file.
    #[arg(value_name = "SOURCE")]
    pub source: String,

    /// Select a displayed 1-based index or an exact torrent path.
    #[arg(long, value_name = "INDEX_OR_NAME")]
    pub file: Option<String>,

    /// Data to prefetch before VLC starts, for example 16MiB or 64MiB.
    #[arg(long, default_value = "16MiB", value_name = "SIZE")]
    pub startup_buffer: String,

    /// Hard limit for materialized torrent pieces, for example 8G or 512MiB.
    #[arg(long, value_name = "SIZE")]
    pub cache_limit: Option<String>,

    /// VLC executable path. Defaults to PATH and platform-standard install locations.
    #[arg(long, value_name = "PATH")]
    pub vlc_path: Option<PathBuf>,

    /// Preserve the session under the output directory after normal exit.
    #[arg(long)]
    pub keep: bool,

    /// Persistent destination used with --keep. Defaults to the user's Videos directory.
    #[arg(long, value_name = "DIRECTORY")]
    pub output: Option<PathBuf>,

    /// Enable diagnostic logging.
    #[arg(long)]
    pub verbose: bool,

    /// Print the stream URL and wait without launching VLC.
    #[arg(long)]
    pub no_launch: bool,

    /// Maximum time to wait for magnet metadata.
    #[arg(long, default_value = "120s", value_name = "DURATION")]
    pub metadata_timeout: String,

    /// Maximum time a missing piece read may remain stalled.
    #[arg(long, default_value = "120s", value_name = "DURATION")]
    pub stall_timeout: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::fs;
    use tempfile::tempdir;

    fn torrent_path() -> (tempfile::TempDir, String) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("fixture.torrent");
        fs::write(&path, b"fixture").unwrap();
        (directory, path.to_string_lossy().into_owned())
    }

    #[test]
    fn fast_startup_buffer_is_the_default() {
        let (_directory, path) = torrent_path();
        let cli = Cli::try_parse_from(["torrent-vlc", &path]).unwrap();
        let config = cli.into_config().unwrap();

        assert_eq!(config.startup_buffer, 16 * 1024 * 1024);
    }

    #[test]
    fn startup_buffer_can_still_be_overridden() {
        let (_directory, path) = torrent_path();
        let cli = Cli::try_parse_from(["torrent-vlc", &path, "--startup-buffer", "64MiB"]).unwrap();
        let config = cli.into_config().unwrap();

        assert_eq!(config.startup_buffer, 64 * 1024 * 1024);
    }
}

#[derive(Debug, Clone)]
pub enum Source {
    Magnet(String),
    TorrentFile(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub source: Source,
    pub file_selector: Option<String>,
    pub startup_buffer: u64,
    pub cache_limit: Option<u64>,
    pub vlc_path: Option<PathBuf>,
    pub keep: bool,
    pub output: Option<PathBuf>,
    pub verbose: bool,
    pub no_launch: bool,
    pub metadata_timeout: Duration,
    pub stall_timeout: Duration,
}

impl Cli {
    pub fn into_config(self) -> Result<Config> {
        let source = if self.source.starts_with("magnet:") {
            let parsed = url::Url::parse(&self.source).context("invalid magnet URI")?;
            if parsed.scheme() != "magnet" || !parsed.query_pairs().any(|(key, _)| key == "xt") {
                bail!("magnet URI must contain an xt parameter");
            }
            Source::Magnet(self.source)
        } else {
            let path = PathBuf::from(self.source);
            if !path.is_file() {
                bail!(
                    "torrent file does not exist or is not a regular file: {}",
                    path.display()
                );
            }
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| !extension.eq_ignore_ascii_case("torrent"))
            {
                bail!(
                    "source must be a magnet URI or a .torrent file: {}",
                    path.display()
                );
            }
            Source::TorrentFile(path)
        };

        let output = self.output.map(|path| {
            if path.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                path
            }
        });
        if output.is_some() && !self.keep {
            bail!("--output requires --keep");
        }

        Ok(Config {
            source,
            file_selector: self.file,
            startup_buffer: parse_byte_size(&self.startup_buffer)
                .context("invalid --startup-buffer")?,
            cache_limit: self
                .cache_limit
                .as_deref()
                .map(parse_byte_size)
                .transpose()
                .context("invalid --cache-limit")?,
            vlc_path: self.vlc_path,
            keep: self.keep,
            output,
            verbose: self.verbose,
            no_launch: self.no_launch,
            metadata_timeout: parse_duration(&self.metadata_timeout)
                .context("invalid --metadata-timeout")?,
            stall_timeout: parse_duration(&self.stall_timeout)
                .context("invalid --stall-timeout")?,
        })
    }
}
