use anyhow::{Context, Result};
use librqbit::spawn_utils::BlockingSpawner;
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, Session, SessionOptions,
    create_torrent,
};
use std::fs;
use std::net::Ipv4Addr;
use std::time::Duration;
use tempfile::tempdir;
use torrent_vlc::stream::prefetch::prefetch;
use torrent_vlc::stream::server::StreamServer;
use torrent_vlc::torrent::layout::{TorrentFile, TorrentLayout};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn localhost_range_stream_matches_a_locally_seeded_torrent() -> Result<()> {
    let fixture_root = tempdir()?;
    let prefix = vec![0x11u8; 37];
    let media = (0..8192u32)
        .map(|value| (value % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(fixture_root.path().join("00-prefix.bin"), &prefix)?;
    fs::write(fixture_root.path().join("01-fixture.mkv"), &media)?;

    let torrent = create_torrent(
        fixture_root.path(),
        CreateTorrentOptions {
            name: Some("local-fixture"),
            piece_length: Some(1024),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await?;
    let torrent_bytes = torrent.as_bytes()?;

    let seed_session = Session::new_with_opts(
        fixture_root.path().to_path_buf(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            ipv4_only: true,
            listen: Some(ListenerOptions {
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await?;
    let seed_handle = seed_session
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes.clone()),
            Some(AddTorrentOptions {
                output_folder: Some(fixture_root.path().to_string_lossy().into_owned()),
                overwrite: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("seed handle missing")?;
    tokio::time::timeout(Duration::from_secs(10), seed_handle.wait_until_completed()).await??;
    let seed_peer = seed_session
        .listen_addr()
        .context("seed listener missing")?;

    let client_root = tempdir()?;
    let client_session = Session::new_with_opts(
        client_root.path().to_path_buf(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            ipv4_only: true,
            ..Default::default()
        },
    )
    .await?;
    let client_handle = client_session
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes),
            Some(AddTorrentOptions {
                only_files: Some(vec![1]),
                initial_peers: Some(vec![seed_peer]),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("client handle missing")?;
    client_handle.wait_until_initialized().await?;

    let (layout, selected) = client_handle.with_metadata(|metadata| {
        let files = metadata
            .file_infos
            .iter()
            .enumerate()
            .map(|(file_id, file)| TorrentFile {
                file_id,
                path: file.relative_filename.to_string_lossy().into_owned(),
                length: file.len,
                torrent_offset: file.offset_in_torrent,
                padding: file.attrs.padding,
            })
            .collect::<Vec<_>>();
        let selected = files[1].clone();
        (
            TorrentLayout {
                piece_length: u64::from(metadata.lengths().default_piece_length()),
                total_length: metadata.lengths().total_length(),
                files,
            },
            selected,
        )
    })?;
    assert_eq!(selected.torrent_offset, prefix.len() as u64);

    let startup_bytes = prefetch(
        client_handle.clone(),
        &selected,
        &layout,
        2_500,
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(startup_bytes, 2_500);

    let mut server =
        StreamServer::start(client_handle, selected, layout, Duration::from_secs(10)).await?;
    let url = server.url();
    let response = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::RANGE, "bytes=500-1700")
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_RANGE],
        "bytes 500-1700/8192"
    );
    let body = response.bytes().await?;
    assert_eq!(body.as_ref(), &media[500..=1700]);

    let head = reqwest::Client::new()
        .head(&url)
        .header(reqwest::header::RANGE, "bytes=500-1700")
        .send()
        .await?;
    assert_eq!(head.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(head.headers()[reqwest::header::CONTENT_LENGTH], "1201");
    assert!(head.bytes().await?.is_empty());

    let invalid = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::RANGE, "bytes=99999-")
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        invalid.headers()[reqwest::header::CONTENT_RANGE],
        "bytes */8192"
    );

    server.stop().await?;
    client_session.stop().await;
    seed_session.stop().await;
    Ok(())
}
