use crate::stream::ranges::{ByteRange, RangePlan, parse_range, plan_range};
use crate::torrent::layout::{TorrentFile, TorrentLayout};
use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use axum::http::{HeaderMap, Method, Response, StatusCode};
use axum::routing::get;
use bytes::Bytes;
use librqbit::ManagedTorrent;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use uuid::Uuid;

#[derive(Clone)]
struct StreamState {
    handle: Arc<ManagedTorrent>,
    selected_file: TorrentFile,
    layout: TorrentLayout,
    token: String,
    stall_timeout: Duration,
    shutdown: CancellationToken,
    fatal: CancellationToken,
}

pub struct StreamServer {
    address: SocketAddr,
    token: String,
    shutdown: CancellationToken,
    fatal: CancellationToken,
    join: Option<JoinHandle<Result<(), std::io::Error>>>,
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl StreamServer {
    pub async fn start(
        handle: Arc<ManagedTorrent>,
        selected_file: TorrentFile,
        layout: TorrentLayout,
        stall_timeout: Duration,
    ) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .context("unable to bind localhost stream server")?;
        let address = listener.local_addr()?;
        let token = Uuid::new_v4().simple().to_string();
        let shutdown = CancellationToken::new();
        let fatal = CancellationToken::new();
        let state = Arc::new(StreamState {
            handle,
            selected_file,
            layout,
            token: token.clone(),
            stall_timeout,
            shutdown: shutdown.clone(),
            fatal: fatal.clone(),
        });
        let router = Router::new()
            .route("/{token}/stream", get(stream_handler).head(stream_handler))
            .with_state(state);
        let server_shutdown = shutdown.clone();
        let server_fatal = fatal.clone();
        let join = tokio::spawn(async move {
            let result = axum::serve(listener, router)
                .with_graceful_shutdown(server_shutdown.cancelled_owned())
                .await;
            if result.is_err() {
                server_fatal.cancel();
            }
            result
        });
        Ok(Self {
            address,
            token,
            shutdown,
            fatal,
            join: Some(join),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}/{}/stream", self.address, self.token)
    }

    pub fn fatal_token(&self) -> CancellationToken {
        self.fatal.clone()
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.shutdown.cancel();
        if let Some(join) = self.join.take() {
            join.await
                .context("localhost stream server task failed")??;
        }
        Ok(())
    }
}

async fn stream_handler(
    State(state): State<Arc<StreamState>>,
    AxumPath(token): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    if token != state.token {
        return plain_error(StatusCode::NOT_FOUND, "not found");
    }

    let range_header = match headers.get(RANGE) {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => return range_not_satisfiable(state.selected_file.length),
        },
        None => None,
    };
    let range = match parse_range(range_header, state.selected_file.length) {
        Ok(Some(range)) => range,
        Ok(None) => ByteRange {
            start: 0,
            end_inclusive: state.selected_file.length - 1,
        },
        Err(error) => {
            debug!(error = %error, "rejected HTTP byte range");
            return range_not_satisfiable(state.selected_file.length);
        }
    };
    let plan = match plan_range(&state.layout, state.selected_file.file_id, range) {
        Ok(plan) => plan,
        Err(error) => {
            tracing::error!(error = %error, "unable to map HTTP range to torrent pieces");
            return plain_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "unable to map stream range",
            );
        }
    };
    debug_range(&plan);

    let status = if range_header.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let content_type = mime_guess::from_path(&state.selected_file.path)
        .first_or_octet_stream()
        .to_string();
    let mut response = Response::builder()
        .status(status)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_LENGTH, range.len().to_string())
        .header(CONTENT_TYPE, content_type);
    if range_header.is_some() {
        response = response.header(
            CONTENT_RANGE,
            format!(
                "bytes {}-{}/{}",
                range.start, range.end_inclusive, state.selected_file.length
            ),
        );
    }
    if method == Method::HEAD {
        return response
            .body(Body::empty())
            .unwrap_or_else(|_| plain_error(StatusCode::INTERNAL_SERVER_ERROR, "response error"));
    }

    let stream_result = tokio::select! {
        _ = state.shutdown.cancelled() => {
            return plain_error(StatusCode::SERVICE_UNAVAILABLE, "stream is shutting down");
        }
        result = state.handle.clone().stream(state.selected_file.file_id) => result,
    };
    let mut file_stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            state.fatal.cancel();
            tracing::error!(error = %error, "unable to open torrent file stream");
            return plain_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "torrent stream unavailable",
            );
        }
    };
    if let Err(error) = file_stream.seek(SeekFrom::Start(range.start)).await {
        state.fatal.cancel();
        tracing::error!(error = %error, "unable to seek torrent file stream");
        return plain_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "torrent stream seek failed",
        );
    }

    let remaining = range.len();
    let read_size = 1024 * 1024;
    let stall_timeout = state.stall_timeout;
    let shutdown = state.shutdown.clone();
    let fatal = state.fatal.clone();
    let body_stream = async_stream::stream! {
        let mut remaining = remaining;
        let mut buffer = vec![0u8; read_size];
        while remaining > 0 {
            let wanted = remaining.min(buffer.len() as u64) as usize;
            let read_result = tokio::select! {
                _ = shutdown.cancelled() => Ok(None),
                read = timeout(stall_timeout, file_stream.read(&mut buffer[..wanted])) => {
                    match read {
                        Ok(Ok(read)) => Ok(Some(read)),
                        Ok(Err(error)) => Err(error),
                        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "torrent piece read stalled")),
                    }
                }
            };
            let read = match read_result {
                Ok(None) => break,
                Ok(Some(read)) => read,
                Err(error) => {
                    fatal.cancel();
                    yield Err(error);
                    break;
                }
            };
            if read == 0 {
                let error = io::Error::new(io::ErrorKind::UnexpectedEof, "torrent file ended before requested range");
                fatal.cancel();
                yield Err(error);
                break;
            }
            remaining -= read as u64;
            yield Ok::<Bytes, io::Error>(Bytes::copy_from_slice(&buffer[..read]));
        }
    };
    response
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| plain_error(StatusCode::INTERNAL_SERVER_ERROR, "response error"))
}

fn debug_range(plan: &RangePlan) {
    debug!(
        file_id = plan.file_id,
        file_start = plan.file_range.start,
        file_end = plan.file_range.end_inclusive,
        torrent_start = plan.torrent_range.start,
        torrent_end = plan.torrent_range.end_exclusive,
        first_piece = plan.first_piece,
        last_piece = plan.last_piece,
        "HTTP range mapped to torrent pieces"
    );
}

fn range_not_satisfiable(total_length: u64) -> Response<Body> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_RANGE, format!("bytes */{total_length}"))
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap()
}

fn plain_error(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CONTENT_LENGTH, message.len().to_string())
        .body(Body::from(message.to_owned()))
        .unwrap()
}
