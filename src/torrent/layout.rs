use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentFile {
    pub file_id: usize,
    pub path: String,
    pub length: u64,
    pub torrent_offset: u64,
    pub padding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentLayout {
    pub piece_length: u64,
    pub total_length: u64,
    pub files: Vec<TorrentFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalRange {
    pub start: u64,
    pub end_exclusive: u64,
}

impl TorrentLayout {
    pub fn file(&self, file_id: usize) -> Result<&TorrentFile> {
        self.files
            .iter()
            .find(|file| file.file_id == file_id)
            .ok_or_else(|| anyhow::anyhow!("torrent file id {file_id} is not present"))
    }

    pub fn global_range(
        &self,
        file_id: usize,
        file_start: u64,
        length: u64,
    ) -> Result<Option<GlobalRange>> {
        let file = self.file(file_id)?;
        let file_end = file_start
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("file range overflows u64"))?;
        if file_end > file.length {
            bail!(
                "file range {}..{} exceeds {} (file {})",
                file_start,
                file_end,
                file.length,
                file.path
            );
        }
        if length == 0 {
            return Ok(None);
        }
        let start = file
            .torrent_offset
            .checked_add(file_start)
            .ok_or_else(|| anyhow::anyhow!("torrent range overflows u64"))?;
        let end_exclusive = start
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("torrent range overflows u64"))?;
        Ok(Some(GlobalRange {
            start,
            end_exclusive,
        }))
    }

    pub fn piece_range(&self, range: GlobalRange) -> Result<Option<(u32, u32)>> {
        if range.start >= range.end_exclusive {
            return Ok(None);
        }
        if range.end_exclusive > self.total_length {
            bail!("torrent range exceeds torrent length");
        }
        if self.piece_length == 0 {
            bail!("torrent piece length is zero");
        }
        let first = range.start / self.piece_length;
        let last = (range.end_exclusive - 1) / self.piece_length;
        Ok(Some((
            u32::try_from(first).map_err(|_| anyhow::anyhow!("piece index is too large"))?,
            u32::try_from(last).map_err(|_| anyhow::anyhow!("piece index is too large"))?,
        )))
    }

    pub fn file_piece_range(
        &self,
        file_id: usize,
        file_start: u64,
        length: u64,
    ) -> Result<Option<(u32, u32)>> {
        self.global_range(file_id, file_start, length)?
            .map(|range| self.piece_range(range))
            .transpose()
            .map(|range| range.flatten())
    }

    pub fn piece_len(&self, piece: u32) -> Result<u64> {
        let start = u64::from(piece)
            .checked_mul(self.piece_length)
            .ok_or_else(|| anyhow::anyhow!("piece offset overflows u64"))?;
        if start >= self.total_length {
            bail!("piece index {piece} is outside torrent");
        }
        Ok((self.total_length - start).min(self.piece_length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> TorrentLayout {
        TorrentLayout {
            piece_length: 16,
            total_length: 64,
            files: vec![
                TorrentFile {
                    file_id: 0,
                    path: "one.mkv".into(),
                    length: 10,
                    torrent_offset: 0,
                    padding: false,
                },
                TorrentFile {
                    file_id: 1,
                    path: "two.mkv".into(),
                    length: 30,
                    torrent_offset: 10,
                    padding: false,
                },
                TorrentFile {
                    file_id: 2,
                    path: "three.mkv".into(),
                    length: 24,
                    torrent_offset: 40,
                    padding: false,
                },
            ],
        }
    }

    #[test]
    fn maps_file_beginning_mid_piece() {
        let result = layout().file_piece_range(1, 0, 30).unwrap();
        assert_eq!(result, Some((0, 2)));
    }

    #[test]
    fn maps_one_piece_and_final_partial_piece() {
        let value = layout();
        assert_eq!(value.file_piece_range(2, 0, 8).unwrap(), Some((2, 2)));
        assert_eq!(value.piece_len(3).unwrap(), 16);
    }
}
