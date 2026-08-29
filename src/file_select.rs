use crate::size::format_bytes;
use crate::torrent::layout::TorrentFile;
use anyhow::{Result, bail};
use std::cmp::Ordering;
use std::io::{self, Write};
use std::path::Path;

const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4", "webm", "avi", "mov", "m4v", "ts", "m2ts"];

pub fn playable_files(files: &[TorrentFile]) -> Vec<TorrentFile> {
    let mut playable = files
        .iter()
        .filter(|file| !file.padding && file.length > 0 && is_playable_path(&file.path))
        .cloned()
        .collect::<Vec<_>>();
    playable.sort_by(|left, right| natural_cmp(&left.path, &right.path));
    playable
}

pub fn select_file(files: &[TorrentFile], selector: Option<&str>) -> Result<TorrentFile> {
    if files.is_empty() {
        bail!("torrent contains no playable media files");
    }

    if let Some(selector) = selector {
        if let Ok(index) = selector.parse::<usize>() {
            if index == 0 || index > files.len() {
                bail!("file index {index} is outside 1..={}", files.len());
            }
            return Ok(files[index - 1].clone());
        }
        if let Some(file) = files.iter().find(|file| {
            file.path == selector
                || Path::new(&file.path).file_name().and_then(|n| n.to_str()) == Some(selector)
        }) {
            return Ok(file.clone());
        }
        bail!("playable file not found: {selector}");
    }

    if files.len() == 1 {
        return Ok(files[0].clone());
    }

    println!("\nVideo files:\n");
    for (index, file) in files.iter().enumerate() {
        println!(
            "{}. {}    {}",
            index + 1,
            file.path,
            format_bytes(file.length)
        );
    }
    print!("\nSelect file: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let index = input
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("please select a numbered file"))?;
    if index == 0 || index > files.len() {
        bail!("file index {index} is outside 1..={}", files.len());
    }
    Ok(files[index - 1].clone())
}

fn is_playable_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            VIDEO_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or(false)
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();

    loop {
        match (left_chars.peek(), right_chars.peek()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char))
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() =>
            {
                let mut left_digits = String::new();
                let mut right_digits = String::new();
                while left_chars
                    .peek()
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    left_digits.push(left_chars.next().unwrap());
                }
                while right_chars
                    .peek()
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    right_digits.push(right_chars.next().unwrap());
                }
                let left_trimmed = left_digits.trim_start_matches('0');
                let right_trimmed = right_digits.trim_start_matches('0');
                let left_number = if left_trimmed.is_empty() {
                    "0"
                } else {
                    left_trimmed
                };
                let right_number = if right_trimmed.is_empty() {
                    "0"
                } else {
                    right_trimmed
                };
                match left_number
                    .len()
                    .cmp(&right_number.len())
                    .then_with(|| left_number.cmp(right_number))
                {
                    Ordering::Equal => {}
                    ordering => return ordering,
                }
            }
            (Some(left_char), Some(right_char)) => {
                let ordering = left_char.cmp(right_char);
                left_chars.next();
                right_chars.next();
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> TorrentFile {
        TorrentFile {
            file_id: 0,
            path: path.into(),
            length: 10,
            torrent_offset: 0,
            padding: false,
        }
    }

    #[test]
    fn filters_and_naturally_sorts_media() {
        let input = vec![
            file("Episode 10.mkv"),
            file("notes.txt"),
            file("Episode 2.mkv"),
            file("Episode 3.MP4"),
        ];
        let result = playable_files(&input);
        assert_eq!(
            result
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["Episode 2.mkv", "Episode 3.MP4", "Episode 10.mkv"]
        );
    }

    #[test]
    fn selects_by_index_or_name() {
        let files = playable_files(&[file("one.mkv"), file("two.mkv")]);
        assert_eq!(select_file(&files, Some("2")).unwrap().path, "two.mkv");
        assert_eq!(
            select_file(&files, Some("one.mkv")).unwrap().path,
            "one.mkv"
        );
    }
}
