use crate::cli::Source;
use crate::torrent::layout::{TorrentFile, TorrentLayout};
use crate::torrent::quota::QuotaStorageFactory;
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerCounts {
    pub connected: u32,
    pub discovered: u32,
    pub connecting: u32,
}

pub struct TorrentEngine {
    pub session: Arc<Session>,
    pub handle: Option<Arc<ManagedTorrent>>,
}

pub struct ResolvedMetadata {
    pub torrent_bytes: Bytes,
    pub info_hash: String,
    pub torrent_name: String,
    pub layout: TorrentLayout,
    /// Peers contacted while resolving a magnet. Reuse them when starting the
    /// actual download instead of forcing a second discovery round.
    pub initial_peers: Vec<SocketAddr>,
}

impl TorrentEngine {
    pub async fn start(root: PathBuf) -> Result<Self> {
        let session = Session::new_with_opts(
            root,
            SessionOptions {
                persistence: None,
                fastresume: false,
                ..SessionOptions::default()
            },
        )
        .await
        .context("unable to start BitTorrent session")?;
        Ok(Self {
            session,
            handle: None,
        })
    }

    pub async fn resolve_metadata(
        &self,
        source: &Source,
        output_folder: &Path,
        metadata_timeout: Duration,
    ) -> Result<ResolvedMetadata> {
        let add = add_from_source(source).await?;
        let response = timeout(
            metadata_timeout,
            self.session.add_torrent(
                add,
                Some(AddTorrentOptions {
                    list_only: true,
                    output_folder: Some(output_folder.to_string_lossy().into_owned()),
                    ..AddTorrentOptions::default()
                }),
            ),
        )
        .await
        .context("timed out waiting for torrent metadata")??;

        let AddTorrentResponse::ListOnly(response) = response else {
            bail!("torrent engine returned an unexpected metadata response");
        };
        let torrent_name = response
            .info
            .name()
            .map(|name| name.into_owned())
            .unwrap_or_else(|| "torrent".to_string());
        let layout = layout_from_info(&response.info)?;
        Ok(ResolvedMetadata {
            torrent_bytes: response.torrent_bytes,
            info_hash: response.info_hash.as_string(),
            torrent_name,
            layout,
            initial_peers: response.seen_peers,
        })
    }

    pub async fn add_selected(
        &mut self,
        torrent_bytes: Bytes,
        output_folder: &Path,
        file_id: usize,
        initial_peers: &[SocketAddr],
        cache_limit: Option<u64>,
    ) -> Result<Arc<ManagedTorrent>> {
        let storage_factory = cache_limit
            .map(QuotaStorageFactory::new)
            .map(|factory| factory.boxed());
        let response = self
            .session
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes),
                Some(AddTorrentOptions {
                    only_files: Some(vec![file_id]),
                    output_folder: Some(output_folder.to_string_lossy().into_owned()),
                    overwrite: false,
                    initial_peers: (!initial_peers.is_empty()).then(|| initial_peers.to_vec()),
                    storage_factory,
                    ..AddTorrentOptions::default()
                }),
            )
            .await
            .context("unable to start selected torrent file")?;
        let handle = response
            .into_handle()
            .context("torrent engine did not return a managed torrent")?;
        handle
            .wait_until_initialized()
            .await
            .context("torrent failed during initialization")?;
        self.handle = Some(handle.clone());
        Ok(handle)
    }

    pub async fn stop(&mut self) {
        self.handle.take();
        self.session.stop().await;
    }
}

pub fn peer_counts(handle: &ManagedTorrent) -> PeerCounts {
    handle
        .stats()
        .live
        .as_ref()
        .map(|live| {
            let stats = &live.snapshot.peer_stats;
            PeerCounts {
                connected: stats.live,
                discovered: stats.seen,
                connecting: stats.connecting,
            }
        })
        .unwrap_or_default()
}

async fn add_from_source(source: &Source) -> Result<AddTorrent<'static>> {
    match source {
        Source::Magnet(uri) => Ok(AddTorrent::from_url(uri.clone())),
        Source::TorrentFile(path) => {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("unable to read torrent file {}", path.display()))?;
            Ok(AddTorrent::from_bytes(bytes))
        }
    }
}

fn layout_from_info(
    info: &librqbit::ValidatedTorrentMetaV1Info<librqbit::ByteBufOwned>,
) -> Result<TorrentLayout> {
    let files = info
        .iter_file_details_ext()
        .enumerate()
        .map(|(file_id, details)| TorrentFile {
            file_id,
            path: details.details.filename.to_string(),
            length: details.details.len,
            torrent_offset: details.offset,
            padding: details.details.attrs().padding,
        })
        .collect::<Vec<_>>();
    Ok(TorrentLayout {
        piece_length: u64::from(info.lengths().default_piece_length()),
        total_length: info.lengths().total_length(),
        files,
    })
}
