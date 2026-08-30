use crate::torrent::layout::{TorrentFile, TorrentLayout};
use anyhow::{Context, Result};
use librqbit::ManagedTorrent;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::time::timeout;
use tracing::debug;

pub async fn prefetch(
    handle: Arc<ManagedTorrent>,
    file: &TorrentFile,
    layout: &TorrentLayout,
    requested_bytes: u64,
    stall_timeout: Duration,
) -> Result<u64> {
    let target = requested_bytes.min(file.length);
    let piece_range = layout
        .file_piece_range(file.file_id, 0, target)
        .context("unable to map startup buffer to torrent pieces")?;
    let started = Instant::now();
    debug!(
        file_id = file.file_id,
        file = %file.path,
        requested_bytes,
        target_bytes = target,
        first_piece = piece_range.map(|(first, _)| first),
        last_piece = piece_range.map(|(_, last)| last),
        "preparing startup buffer"
    );
    if target == 0 {
        return Ok(0);
    }
    let mut stream = handle
        .stream(file.file_id)
        .await
        .context("unable to create startup torrent stream")?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut read_total = 0u64;
    let mut first_data_wait = None;
    while read_total < target {
        let wanted = (target - read_total).min(buffer.len() as u64) as usize;
        let read = timeout(stall_timeout, stream.read(&mut buffer[..wanted]))
            .await
            .context("startup buffer stalled while waiting for torrent pieces")?
            .context("startup buffer read failed")?;
        if read == 0 {
            break;
        }
        if first_data_wait.is_none() {
            let elapsed = started.elapsed();
            first_data_wait = Some(elapsed);
            debug!(wait = ?elapsed, "first startup data is ready");
        }
        read_total += read as u64;
    }
    if read_total < target {
        anyhow::bail!("startup stream ended before the requested buffer was ready");
    }
    debug!(
        bytes = read_total,
        elapsed = ?started.elapsed(),
        first_data_wait = ?first_data_wait,
        "startup buffer ready"
    );
    Ok(read_total)
}
