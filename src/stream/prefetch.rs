use crate::torrent::layout::TorrentFile;
use anyhow::{Context, Result};
use librqbit::ManagedTorrent;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

pub async fn prefetch(
    handle: Arc<ManagedTorrent>,
    file: &TorrentFile,
    requested_bytes: u64,
    stall_timeout: Duration,
) -> Result<u64> {
    let target = requested_bytes.min(file.length);
    if target == 0 {
        return Ok(0);
    }
    let mut stream = handle
        .stream(file.file_id)
        .await
        .context("unable to create startup torrent stream")?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut read_total = 0u64;
    while read_total < target {
        let wanted = (target - read_total).min(buffer.len() as u64) as usize;
        let read = timeout(stall_timeout, stream.read(&mut buffer[..wanted]))
            .await
            .context("startup buffer stalled while waiting for torrent pieces")?
            .context("startup buffer read failed")?;
        if read == 0 {
            break;
        }
        read_total += read as u64;
    }
    if read_total < target {
        anyhow::bail!("startup stream ended before the requested buffer was ready");
    }
    Ok(read_total)
}
