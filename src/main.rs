use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Child;
use tokio::task::JoinHandle;
use torrent_vlc::cli::{Cli, Config, Source};
use torrent_vlc::file_select::{is_playable_path, playable_files, select_file, select_file_id};
use torrent_vlc::player::vlc::{launch, terminate, validate_vlc_path};
use torrent_vlc::session::recovery::{default_cache_root, recover_stale_sessions};
use torrent_vlc::session::{SessionGuard, sanitize_name};
use torrent_vlc::stream::prefetch::prefetch;
use torrent_vlc::stream::server::StreamServer;
use torrent_vlc::torrent::engine::{PeerCounts, TorrentEngine, peer_counts};
use tracing_subscriber::EnvFilter;

const PEER_STATUS_REFRESH: Duration = Duration::from_millis(500);
const PEER_STATUS_LINE_WIDTH: usize = 80;

struct PeerStatusDisplay {
    task: Option<JoinHandle<()>>,
    interactive: bool,
}

impl PeerStatusDisplay {
    fn start(handle: Arc<librqbit::ManagedTorrent>) -> Self {
        let interactive = io::stdout().is_terminal()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
        if !interactive {
            return Self {
                task: None,
                interactive: false,
            };
        }

        render_peer_status(peer_counts(handle.as_ref()));
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PEER_STATUS_REFRESH);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                render_peer_status(peer_counts(handle.as_ref()));
            }
        });
        Self {
            task: Some(task),
            interactive: true,
        }
    }

    async fn finish(&mut self, counts: PeerCounts) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        if self.interactive {
            render_peer_status(counts);
            clear_status_line();
        }
        println!(
            "Peers: {} discovered, {} connected",
            counts.discovered, counts.connected
        );
    }
}

fn render_peer_status(counts: PeerCounts) {
    let mut stdout = io::stdout().lock();
    let line = format!(
        "Peers: {} discovered, {} connected",
        counts.discovered, counts.connected
    );
    let _ = write!(stdout, "\r{line:<width$}", width = PEER_STATUS_LINE_WIDTH);
    let _ = stdout.flush();
}

fn clear_status_line() {
    let mut stdout = io::stdout().lock();
    let _ = write!(stdout, "\r{:width$}\r", "", width = PEER_STATUS_LINE_WIDTH);
    let _ = stdout.flush();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunOutcome {
    PlaybackEnded,
    Interrupted,
}

struct RuntimeSession {
    guard: Option<SessionGuard>,
    engine: Option<TorrentEngine>,
    server: Option<StreamServer>,
    persistent_name: Option<String>,
}

impl RuntimeSession {
    async fn create(sessions_root: &Path) -> Result<Self> {
        let guard = SessionGuard::create(sessions_root.to_path_buf())?;
        let engine = match TorrentEngine::start(guard.data_path()).await {
            Ok(engine) => engine,
            Err(error) => {
                // Dropping the guard here performs best-effort cleanup of the
                // session that was created before the engine failed.
                drop(guard);
                return Err(error);
            }
        };
        Ok(Self {
            guard: Some(guard),
            engine: Some(engine),
            server: None,
            persistent_name: None,
        })
    }

    async fn execute(&mut self, config: &Config) -> Result<RunOutcome> {
        println!("Loading torrent metadata...");
        let guard = self.guard.as_ref().context("session guard is missing")?;
        let engine = self.engine.as_mut().context("torrent engine is missing")?;
        let metadata = engine
            .resolve_metadata(&config.source, &guard.data_path(), config.metadata_timeout)
            .await?;
        guard.write_metadata_file("torrent.torrent", &metadata.torrent_bytes)?;
        println!("\nTorrent metadata loaded.");

        let candidates = playable_files(&metadata.layout.files);
        let selected = if let Some(file_id) = config.file_id {
            select_file_id(&candidates, file_id)?
        } else {
            select_file(&candidates, config.file_selector.as_deref())?
        };
        let selected_name = selected.path.clone();
        let selection = serde_json::json!({
            "file_id": selected.file_id,
            "path": selected.path,
            "length": selected.length,
            "torrent_offset": selected.torrent_offset,
        });
        guard.write_metadata_file(
            "selection.json",
            serde_json::to_string_pretty(&selection)?.as_bytes(),
        )?;

        let handle = engine
            .add_selected(
                metadata.torrent_bytes.clone(),
                &guard.data_path(),
                selected.file_id,
                &metadata.initial_peers,
                config.cache_limit,
            )
            .await?;
        println!("Streaming {selected_name}");
        println!("Preparing stream...");

        let server = StreamServer::start(
            handle.clone(),
            selected.clone(),
            metadata.layout.clone(),
            config.stall_timeout,
        )
        .await?;
        let url = server.url();
        self.server = Some(server);
        let mut buffer_display = PeerStatusDisplay::start(handle.clone());
        let stats_handle = handle.clone();
        let prefetch_result = prefetch(
            handle,
            &selected,
            &metadata.layout,
            config.startup_buffer,
            config.stall_timeout,
            config.startup_timeout,
        )
        .await;
        buffer_display
            .finish(peer_counts(stats_handle.as_ref()))
            .await;
        prefetch_result.context("unable to prepare startup buffer")?;
        println!("Buffer ready.");

        let safe_torrent_name = sanitize_name(&metadata.torrent_name);
        let safe_selected_name = sanitize_name(
            Path::new(&selected.path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("media"),
        );
        let display_name = if safe_torrent_name.is_empty() || safe_torrent_name == "torrent" {
            safe_selected_name
        } else {
            safe_torrent_name
        };
        let short_hash = metadata.info_hash.chars().take(8).collect::<String>();
        self.persistent_name = Some(format!("{display_name}-{short_hash}"));

        if config.no_launch {
            println!("\nStream URL: {url}");
            println!("Press Ctrl+C to stop.");
            let fatal = self.server.as_ref().unwrap().fatal_token();
            tokio::select! {
                result = wait_for_shutdown_signal() => {
                    result?;
                    Ok(RunOutcome::Interrupted)
                }
                _ = fatal.cancelled() => {
                    bail!("stream server stopped after a torrent read failure")
                }
            }
        } else {
            println!("\nOpening VLC...");
            let mut child = launch(config.vlc_path.as_deref(), &url, config.verbose).await?;
            self.wait_for_vlc(&mut child).await
        }
    }

    async fn wait_for_vlc(&self, child: &mut Child) -> Result<RunOutcome> {
        let fatal = self
            .server
            .as_ref()
            .context("stream server is missing")?
            .fatal_token();
        tokio::select! {
            status = child.wait() => {
                let status = status.context("unable to wait for VLC")?;
                if !status.success() {
                    bail!("VLC exited unsuccessfully with status {status}");
                }
                Ok(RunOutcome::PlaybackEnded)
            }
            result = wait_for_shutdown_signal() => {
                result?;
                terminate(child).await?;
                Ok(RunOutcome::Interrupted)
            }
            _ = fatal.cancelled() => {
                terminate(child).await?;
                bail!("stream stopped after a torrent read failure")
            }
        }
    }

    async fn shutdown(
        &mut self,
        preserve: bool,
        output_root: Option<PathBuf>,
    ) -> Result<Option<PathBuf>> {
        let mut first_error = None;
        if let Some(server) = self.server.as_mut()
            && let Err(error) = server.stop().await
        {
            first_error = Some(error);
        }
        self.server.take();

        if let Some(engine) = self.engine.as_mut() {
            engine.stop().await;
        }
        self.engine.take();

        let result = if let Some(guard) = self.guard.as_mut() {
            if preserve {
                let output_root = output_root.context("persistent output directory is missing")?;
                let name = self
                    .persistent_name
                    .as_deref()
                    .unwrap_or("torrent-vlc-session");
                guard.preserve_data(&output_root, name).await.map(Some)
            } else {
                guard.cleanup().await.map(|()| None)
            }
        } else {
            Ok(None)
        };
        self.guard.take();
        if let Some(error) = first_error {
            return Err(error);
        }
        result
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "torrent-vlc inspect",
    about = "Inspect authorized torrent metadata as JSON"
)]
struct InspectCli {
    /// A magnet URI or a local .torrent file.
    #[arg(value_name = "SOURCE")]
    source: String,

    /// Accepted for explicit machine-readable invocation; inspect is always JSON.
    #[arg(long)]
    json: bool,

    /// Maximum time to wait for magnet metadata.
    #[arg(long, default_value = "120s", value_name = "DURATION")]
    metadata_timeout: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectOutput {
    torrent_name: String,
    info_hash: String,
    total_size: u64,
    files: Vec<InspectFile>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectFile {
    index: usize,
    name: String,
    size: u64,
    playable: bool,
    padding: bool,
    playable_ordinal: Option<usize>,
}

async fn inspect(cli: InspectCli) -> Result<()> {
    let source = parse_inspect_source(&cli.source)?;
    let metadata_timeout = torrent_vlc::size::parse_duration(&cli.metadata_timeout)
        .context("invalid --metadata-timeout")?;
    let cache_root = default_cache_root();
    let sessions_root = cache_root.join("sessions");
    let mut runtime = RuntimeSession::create(&sessions_root).await?;
    let result = async {
        let guard = runtime.guard.as_ref().context("session guard is missing")?;
        let engine = runtime
            .engine
            .as_ref()
            .context("torrent engine is missing")?;
        let metadata = engine
            .resolve_metadata(&source, &guard.data_path(), metadata_timeout)
            .await?;
        let playable_ids = playable_files(&metadata.layout.files)
            .into_iter()
            .enumerate()
            .map(|(index, file)| (file.file_id, index + 1))
            .collect::<std::collections::HashMap<usize, usize>>();
        let files = metadata
            .layout
            .files
            .iter()
            .map(|file| InspectFile {
                index: file.file_id,
                name: file.path.clone(),
                size: file.length,
                playable: !file.padding && file.length > 0 && is_playable_path(&file.path),
                padding: file.padding,
                playable_ordinal: playable_ids.get(&file.file_id).copied(),
            })
            .collect::<Vec<_>>();
        let output = InspectOutput {
            torrent_name: metadata.torrent_name,
            info_hash: metadata.info_hash,
            total_size: metadata.layout.total_length,
            files,
        };
        Ok::<_, anyhow::Error>(serde_json::to_string_pretty(&output)?)
    }
    .await;
    let cleanup = runtime.shutdown(false, None).await;
    let json = result?;
    cleanup?;
    println!("{json}");
    Ok(())
}

fn parse_inspect_source(source: &str) -> Result<Source> {
    if source.starts_with("magnet:") {
        let parsed = url::Url::parse(source).context("invalid magnet URI")?;
        if parsed.scheme() != "magnet" || !parsed.query_pairs().any(|(key, _)| key == "xt") {
            bail!("magnet URI must contain an xt parameter");
        }
        return Ok(Source::Magnet(source.to_string()));
    }
    let path = PathBuf::from(source);
    if !path.is_file()
        || !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
    {
        bail!(
            "source must be a magnet URI or a .torrent file: {}",
            path.display()
        );
    }
    Ok(Source::TorrentFile(path))
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut arguments = std::env::args();
    let _program = arguments.next();
    if arguments.next().as_deref() == Some("inspect") {
        let mut inspect_arguments = vec!["torrent-vlc inspect".to_string()];
        inspect_arguments.extend(arguments);
        let cli = InspectCli::parse_from(inspect_arguments);
        init_logging(false);
        return inspect(cli).await;
    }
    let cli = Cli::parse();
    let config = cli.into_config()?;
    init_logging(config.verbose);
    if !config.no_launch {
        validate_vlc_path(config.vlc_path.as_deref())?;
    }

    let cache_root = default_cache_root();
    let sessions_root = cache_root.join("sessions");
    let removed = recover_stale_sessions(&sessions_root)?;
    if config.verbose && !removed.is_empty() {
        tracing::debug!(count = removed.len(), "removed stale torrent-vlc sessions");
    }

    let mut runtime = RuntimeSession::create(&sessions_root).await?;
    let outcome = runtime.execute(&config).await;
    match outcome {
        Ok(outcome) => {
            if outcome == RunOutcome::PlaybackEnded {
                println!("\nPlayback ended.");
            } else {
                println!("\nStopping playback.");
            }
            println!("Cleaning temporary files...");
            let preserve = config.keep;
            let output_root = if preserve {
                match persistent_output_root(&config) {
                    Ok(path) => Some(path),
                    Err(error) => {
                        let _ = runtime.shutdown(false, None).await;
                        return Err(error);
                    }
                }
            } else {
                None
            };
            let preserved = runtime.shutdown(preserve, output_root).await?;
            if let Some(path) = preserved {
                println!("Kept downloaded data at {}.", path.display());
            } else {
                println!("Done.");
            }
            Ok(())
        }
        Err(error) => {
            let cleanup_result = runtime.shutdown(false, None).await;
            if let Err(cleanup_error) = cleanup_result {
                return Err(error.context(format!("cleanup also failed: {cleanup_error:#}")));
            }
            Err(error)
        }
    }
}

fn init_logging(verbose: bool) {
    let default_filter = if verbose {
        "torrent_vlc=debug,librqbit=info"
    } else {
        "torrent_vlc=info,librqbit=warn"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

fn persistent_output_root(config: &Config) -> Result<PathBuf> {
    if let Some(output) = &config.output {
        return Ok(output.clone());
    }
    if let Some(user_dirs) = directories::UserDirs::new()
        && let Some(video_dir) = user_dirs.video_dir()
    {
        return Ok(video_dir.to_path_buf());
    }
    Ok(std::env::current_dir()
        .context("unable to determine a default output directory")?
        .join("Videos"))
}

async fn wait_for_shutdown_signal() -> Result<()> {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("unable to install SIGTERM handler")?;
        tokio::select! {
            result = ctrl_c => result.context("unable to wait for Ctrl+C")?,
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.context("unable to wait for Ctrl+C")?;
    }
    Ok(())
}
