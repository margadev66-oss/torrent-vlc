use anyhow::{Context, Result};
use librqbit::spawn_utils::BlockingSpawner;
use librqbit::{CreateTorrentOptions, create_torrent};
use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[tokio::test]
async fn inspect_reports_raw_file_ids_and_playable_media() -> Result<()> {
    let fixture_root = tempdir()?;
    fs::create_dir(fixture_root.path().join("subtitles"))?;
    fs::write(fixture_root.path().join("Anime - 01.mkv"), [0u8; 128])?;
    fs::write(fixture_root.path().join("Anime - 02.mkv"), [1u8; 128])?;
    fs::write(fixture_root.path().join("Anime - NCOP.mkv"), [2u8; 128])?;
    fs::write(fixture_root.path().join("cover.jpg"), [3u8; 16])?;
    fs::write(
        fixture_root.path().join("subtitles").join("01.ass"),
        [4u8; 16],
    )?;

    let torrent = create_torrent(
        fixture_root.path(),
        CreateTorrentOptions {
            name: Some("yorumi-inspect-fixture"),
            piece_length: Some(64),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await?;
    let torrent_path = fixture_root.path().join("fixture.torrent");
    fs::write(&torrent_path, torrent.as_bytes()?)?;

    let output = Command::new(env!("CARGO_BIN_EXE_torrent-vlc"))
        .args([
            "inspect",
            torrent_path.to_str().context("fixture path is not UTF-8")?,
            "--json",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: Value = serde_json::from_slice(&output.stdout)?;
    let files = payload["files"]
        .as_array()
        .context("inspect files are missing")?;
    let episode_one = files
        .iter()
        .find(|file| file["name"] == "Anime - 01.mkv")
        .context("episode 01 is missing")?;
    let episode_two = files
        .iter()
        .find(|file| file["name"] == "Anime - 02.mkv")
        .context("episode 02 is missing")?;
    let cover = files
        .iter()
        .find(|file| file["name"] == "cover.jpg")
        .context("cover is missing")?;

    assert!(episode_one["playable"].as_bool().unwrap_or(false));
    assert!(episode_two["playable"].as_bool().unwrap_or(false));
    assert_eq!(cover["playable"], false);
    assert_ne!(episode_one["index"], episode_two["index"]);
    assert!(episode_one["playableOrdinal"].is_number());
    assert!(episode_two["playableOrdinal"].is_number());
    Ok(())
}
