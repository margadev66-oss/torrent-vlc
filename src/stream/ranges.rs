use crate::torrent::layout::{GlobalRange, TorrentLayout};
use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end_inclusive: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangePlan {
    pub file_id: usize,
    pub file_range: ByteRange,
    pub torrent_range: GlobalRange,
    pub first_piece: u32,
    pub last_piece: u32,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end_inclusive - self.start + 1
    }

    pub fn is_empty(self) -> bool {
        self.start > self.end_inclusive
    }
}

pub fn plan_range(layout: &TorrentLayout, file_id: usize, range: ByteRange) -> Result<RangePlan> {
    let torrent_range = layout
        .global_range(file_id, range.start, range.len())?
        .ok_or_else(|| anyhow::anyhow!("cannot plan an empty HTTP range"))?;
    let (first_piece, last_piece) = layout
        .piece_range(torrent_range)?
        .ok_or_else(|| anyhow::anyhow!("cannot map an empty torrent range"))?;
    Ok(RangePlan {
        file_id,
        file_range: range,
        torrent_range,
        first_piece,
        last_piece,
    })
}

pub fn parse_range(header: Option<&str>, total_length: u64) -> Result<Option<ByteRange>> {
    let Some(header) = header else {
        return Ok(None);
    };
    if total_length == 0 {
        bail!("range is unsatisfiable for an empty file");
    }

    let Some(value) = header.trim().strip_prefix("bytes=") else {
        bail!("only byte ranges are supported");
    };
    if value.contains(',') {
        bail!("multiple byte ranges are not supported");
    }
    let value = value.trim();
    let Some((start, end)) = value.trim().split_once('-') else {
        bail!("malformed byte range");
    };
    if start.is_empty() {
        let suffix = end
            .trim()
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("invalid suffix byte range"))?;
        if suffix == 0 {
            bail!("suffix byte range cannot be zero");
        }
        let length = suffix.min(total_length);
        return Ok(Some(ByteRange {
            start: total_length - length,
            end_inclusive: total_length - 1,
        }));
    }

    let start = start
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid range start"))?;
    if start >= total_length {
        bail!("range start is outside file");
    }
    let end = if end.trim().is_empty() {
        total_length - 1
    } else {
        let requested_end = end
            .trim()
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("invalid range end"))?;
        if requested_end < start {
            bail!("range end precedes range start");
        }
        requested_end.min(total_length - 1)
    };
    Ok(Some(ByteRange {
        start,
        end_inclusive: end,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_and_closed_ranges() {
        assert_eq!(
            parse_range(Some("bytes=0-"), 1_000).unwrap(),
            Some(ByteRange {
                start: 0,
                end_inclusive: 999
            })
        );
        assert_eq!(
            parse_range(Some("bytes=100-200"), 1_000).unwrap(),
            Some(ByteRange {
                start: 100,
                end_inclusive: 200
            })
        );
        assert_eq!(
            parse_range(Some("bytes=500-"), 1_000).unwrap(),
            Some(ByteRange {
                start: 500,
                end_inclusive: 999
            })
        );
    }

    #[test]
    fn parses_suffix_and_clamps_end() {
        assert_eq!(
            parse_range(Some("bytes=-100"), 50).unwrap(),
            Some(ByteRange {
                start: 0,
                end_inclusive: 49
            })
        );
        assert_eq!(
            parse_range(Some("bytes=10-999"), 50).unwrap(),
            Some(ByteRange {
                start: 10,
                end_inclusive: 49
            })
        );
    }

    #[test]
    fn rejects_invalid_ranges() {
        for value in [
            "items=0-",
            "bytes=",
            "bytes=10-2",
            "bytes=100-",
            "bytes=0-1,2-3",
        ] {
            assert!(parse_range(Some(value), 100).is_err(), "{value}");
        }
    }

    #[test]
    fn plans_a_range_through_a_file_that_starts_mid_piece() {
        let layout = TorrentLayout {
            piece_length: 16,
            total_length: 32,
            files: vec![
                crate::torrent::layout::TorrentFile {
                    file_id: 0,
                    path: "a.mkv".into(),
                    length: 5,
                    torrent_offset: 0,
                    padding: false,
                },
                crate::torrent::layout::TorrentFile {
                    file_id: 1,
                    path: "b.mkv".into(),
                    length: 27,
                    torrent_offset: 5,
                    padding: false,
                },
            ],
        };
        let plan = plan_range(
            &layout,
            1,
            ByteRange {
                start: 0,
                end_inclusive: 20,
            },
        )
        .unwrap();
        assert_eq!(plan.torrent_range.start, 5);
        assert_eq!((plan.first_piece, plan.last_piece), (0, 1));
    }
}
