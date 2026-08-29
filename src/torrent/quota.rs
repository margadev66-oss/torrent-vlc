use anyhow::{Context, Result, bail};
use librqbit::storage::filesystem::FilesystemStorageFactory;
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use std::collections::HashSet;
use std::io::IoSlice;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct QuotaStorageFactory {
    limit: u64,
}

impl QuotaStorageFactory {
    pub fn new(limit: u64) -> Self {
        Self { limit }
    }

    pub fn boxed(self) -> BoxStorageFactory {
        StorageFactoryExt::boxed(self)
    }
}

impl StorageFactory for QuotaStorageFactory {
    type Storage = QuotaStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> Result<Self::Storage> {
        let inner = FilesystemStorageFactory::default().create(shared, metadata)?;
        let file_offsets = metadata
            .file_infos
            .iter()
            .map(|file| (file.offset_in_torrent, file.len))
            .collect();
        Ok(QuotaStorage {
            inner: Box::new(inner),
            state: Arc::new(Mutex::new(QuotaState::default())),
            limit: self.limit,
            piece_length: u64::from(metadata.lengths().default_piece_length()),
            total_length: metadata.lengths().total_length(),
            file_offsets,
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

#[derive(Default)]
struct QuotaState {
    reserved_bytes: u64,
    pieces: HashSet<u32>,
}

pub struct QuotaStorage {
    inner: Box<dyn TorrentStorage>,
    state: Arc<Mutex<QuotaState>>,
    limit: u64,
    piece_length: u64,
    total_length: u64,
    file_offsets: Vec<(u64, u64)>,
}

impl QuotaStorage {
    fn piece_len(&self, piece: u32) -> u64 {
        let start = u64::from(piece) * self.piece_length;
        (self.total_length.saturating_sub(start)).min(self.piece_length)
    }

    fn reserve(&self, file_id: usize, offset: u64, length: usize) -> Result<Vec<u32>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let (file_offset, file_length) = self
            .file_offsets
            .get(file_id)
            .copied()
            .context("quota storage received an invalid file id")?;
        let end = offset
            .checked_add(length as u64)
            .context("quota storage write offset overflow")?;
        if end > file_length {
            bail!("quota storage write exceeds torrent file length");
        }
        let global_start = file_offset
            .checked_add(offset)
            .context("quota storage global offset overflow")?;
        let global_end = global_start
            .checked_add(length as u64)
            .context("quota storage global range overflow")?;
        let first = global_start / self.piece_length;
        let last = (global_end - 1) / self.piece_length;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("quota state lock poisoned"))?;
        let new_pieces = (first..=last)
            .map(|piece| u32::try_from(piece).context("torrent has too many pieces"))
            .collect::<Result<Vec<_>>>()?;
        let additional = new_pieces
            .iter()
            .filter(|piece| !state.pieces.contains(piece))
            .map(|piece| self.piece_len(*piece))
            .sum::<u64>();
        if state.reserved_bytes.saturating_add(additional) > self.limit {
            bail!(
                "cache limit of {} bytes would be exceeded by this torrent piece write",
                self.limit
            );
        }
        for piece in &new_pieces {
            if state.pieces.insert(*piece) {
                state.reserved_bytes += self.piece_len(*piece);
            }
        }
        Ok(new_pieces
            .into_iter()
            .filter(|piece| state.pieces.contains(piece))
            .collect())
    }
}

impl TorrentStorage for QuotaStorage {
    fn init(&mut self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<()> {
        self.inner.init(shared, metadata)
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.pread_exact(file_id, offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> Result<()> {
        let _reserved_pieces = self.reserve(file_id, offset, buf.len())?;
        self.inner.pwrite_all(file_id, offset, buf)
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> Result<usize> {
        // Reserve the complete contiguous span once so a piece split between the two
        // buffers cannot bypass the hard quota.
        let total = bufs.iter().map(|buf| buf.len()).sum::<usize>();
        let _reserved_pieces = self.reserve(file_id, offset, total)?;
        self.inner.pwrite_all_vectored(file_id, offset, bufs)
    }

    fn remove_file(&self, file_id: usize, filename: &Path) -> Result<()> {
        self.inner.remove_file(file_id, filename)
    }

    fn remove_directory_if_empty(&self, path: &Path) -> Result<()> {
        self.inner.remove_directory_if_empty(path)
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> Result<()> {
        self.inner.ensure_file_length(file_id, length)
    }

    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(Self {
            inner: self.inner.take()?,
            state: self.state.clone(),
            limit: self.limit,
            piece_length: self.piece_length,
            total_length: self.total_length,
            file_offsets: self.file_offsets.clone(),
        }))
    }
}
