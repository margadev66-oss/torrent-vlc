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
    startup_timeout: Duration,
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
    if stall_timeout.is_zero() {
        anyhow::bail!("startup stall timeout must be greater than zero");
    }
    if startup_timeout.is_zero() {
        anyhow::bail!("startup timeout must be greater than zero");
    }
    let mut stream = handle
        .stream(file.file_id)
        .await
        .context("unable to create startup torrent stream")?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut read_total = 0u64;
    let mut first_data_wait = None;
    let mut stall_retries = 0u32;
    while read_total < target {
        let remaining_timeout = startup_timeout.saturating_sub(started.elapsed());
        if remaining_timeout.is_zero() {
            anyhow::bail!(
                "startup buffer timed out after {startup_timeout:?} while waiting for torrent pieces (received {read_total} of {target} bytes); try a smaller --startup-buffer, a longer --startup-timeout, or verify that peers have the selected file"
            );
        }
        let wanted = (target - read_total).min(buffer.len() as u64) as usize;
        let read_timeout = stall_timeout.min(remaining_timeout);
        let read = match timeout(read_timeout, stream.read(&mut buffer[..wanted])).await {
            Ok(read) => read.context("startup buffer read failed")?,
            Err(_) => {
                stall_retries += 1;
                debug!(
                    retries = stall_retries,
                    bytes = read_total,
                    target,
                    "startup read stalled; retrying while peers continue downloading"
                );
                continue;
            }
        };
        if read == 0 {
            break;
        }
        if first_data_wait.is_none() {
            let elapsed = started.elapsed();
            first_data_wait = Some(elapsed);
            debug!(wait = ?elapsed, "first startup data is ready");
        }
        read_total += read as u64;
        stall_retries = 0;
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
