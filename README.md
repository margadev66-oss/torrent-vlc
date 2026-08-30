# torrent-vlc

`torrent-vlc` is a small Linux-first command-line client for streaming a media
file from an authorized BitTorrent torrent into VLC. VLC remains the media
player; this program only resolves and downloads torrent pieces, serves a
localhost byte-range stream, launches VLC, and cleans up the temporary session.

Use it only for torrents you are authorized to access. The project does not
search, index, scrape, discover, or integrate with media providers.

## Requirements

- Rust stable (1.98 or newer is recommended for the current lockfile)
- VLC available as `vlc`, or an explicit `--vlc-path`

## Run

```bash
cargo install --path .
cargo run --release -- --help
cargo run --release -- "magnet:?xt=urn:btih:..."
cargo run --release -- example.torrent --startup-buffer 128M
```

The program lists playable video files when a torrent contains more than one.
Select one interactively, or use `--file 2` or `--file 'path/in/torrent.mkv'`.
It starts VLC before the selected file is complete and leaves playback controls,
seeking, audio, subtitles, and hardware decoding to VLC.

## Options

```text
--file <INDEX_OR_NAME>       Select a displayed 1-based index or exact path
--startup-buffer <SIZE>      Initial prefetch; default 16MiB
--cache-limit <SIZE>         Hard limit for materialized torrent pieces
--vlc-path <PATH>            VLC executable override
--keep                       Preserve downloaded data after normal exit
--output <DIRECTORY>         Persistent destination used with --keep
--verbose                    Show diagnostic torrent/HTTP logging
--no-launch                  Print the localhost URL and wait for Ctrl+C
--metadata-timeout <TIME>    Magnet metadata timeout; default 120s
--stall-timeout <TIME>       Per-read missing-piece timeout; default 120s
```

`--keep` defaults to the user's Videos directory. Without `--keep`, the unique
session directory and all downloaded data are deleted when VLC exits or the
session is interrupted. A kept session may be partial if playback was stopped
before the torrent finished. The cache limit is a hard materialized-piece
quota; V1 does not evict already downloaded pieces.

The local HTTP server binds to `127.0.0.1` on a random port and includes a
random per-session URL token. It supports normal requests, `HEAD`, open-ended
ranges, closed ranges, and suffix ranges. Range reads are mapped across the
torrent's global piece layout, including files that start in the middle of a
piece. `librqbit`'s file stream keeps the currently requested and forward
playback regions prioritized, so VLC seeks remain random-access operations.

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

The unit tests cover size parsing, natural media sorting, range semantics,
multi-file byte-to-piece mapping, session cleanup, and stale-session ownership.
The streaming implementation is intentionally kept behind VLC and the torrent
engine; automated VLC playback is not required for CI.
