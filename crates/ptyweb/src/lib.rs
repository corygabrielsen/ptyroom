//! Browser ↔ ptyroom WebSocket bridge.
//!
//! ptyweb is a single-room-per-process bridge. It connects to one
//! ptyroom host over TCP and serves one WebSocket endpoint that
//! browsers can attach to. Multi-room concerns (registry, routing,
//! per-user auth) belong to the orchestrator that spawns ptyweb,
//! not to ptyweb itself.
//!
//! ## Wire shapes
//!
//! WebSocket frames between browser and ptyweb:
//!
//! - browser → ptyweb: binary = raw keystroke bytes; text = JSON
//!   `{"resize": {"cols": u16, "rows": u16}}`
//! - ptyweb → browser: binary = raw PTY output bytes; text = JSON
//!   (reserved for advisory status / resize echoes)
//!
//! ## Auth
//!
//! Production deployments sit behind a reverse proxy that injects
//! `X-PtyWeb-Auth: <secret>` on forwarded requests. ptyweb verifies
//! the header matches [`Config::auth_secret`]. If no secret is
//! configured, ptyweb refuses non-loopback connections.
//!
//! ## Read-only mode
//!
//! When [`Config::read_only`] is true, ptyweb drops every browser
//! frame that would touch the PTY (keystrokes and resize controls).
//! Browsers still see PTY output as it arrives.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use ptyroom::protocol::{self, TerminalSize};
use ptyroom::stream::{ServerEvent, ServerStream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

/// Static viewer assets, baked into the binary so ptyweb has no
/// runtime CDN dependency.
pub const VIEWER_INDEX_HTML: &str = include_str!("viewer/index.html");
pub const VIEWER_JS: &str = include_str!("viewer/viewer.js");
pub const XTERM_JS: &str = include_str!("viewer/xterm.js");
pub const XTERM_CSS: &str = include_str!("viewer/xterm.css");
/// `xterm-addon-fit` 0.8.0 (MIT, <https://github.com/xtermjs/xterm.js>).
/// Pairs with the vendored xterm.js 5.3.0; sizes the terminal grid to
/// the host element instead of guessing from a fixed cell estimate.
pub const XTERM_ADDON_FIT_JS: &str = include_str!("viewer/xterm-addon-fit.js");

/// HTTP header the reverse proxy uses to authenticate to ptyweb.
pub const AUTH_HEADER: &str = "X-PtyWeb-Auth";

/// Channel depth for both directions of the WS↔TCP bridge. Generous
/// enough to absorb burst writes without back-pressuring the
/// in-process tasks; small enough to keep memory bounded if a peer
/// stalls.
const CHANNEL_DEPTH: usize = 256;

/// TCP read buffer size. Matches the join client's framing buffer.
const TCP_READ_BUF: usize = 16 * 1024;

/// Upper bound on browser-reported resize dimensions. Mirrors
/// `ptyrender::frame_replay::MAX_TRACE_DIM` so the renderer doesn't
/// reject sizes the bridge happily accepted.
const MAX_RESIZE_DIM: u16 = 1024;

#[derive(Debug, Clone)]
pub struct Config {
    /// TCP address of the ptyroom host to bridge.
    pub room_addr: SocketAddr,
    /// WebSocket listener address.
    pub listen_addr: SocketAddr,
    /// Shared secret the reverse proxy injects via [`AUTH_HEADER`].
    /// When `None`, non-loopback connections are refused.
    pub auth_secret: Option<String>,
    /// Value for `Access-Control-Allow-Origin`. When `None`, the
    /// header is omitted entirely.
    pub allowed_origin: Option<String>,
    /// Drop browser-originated bytes instead of forwarding to the
    /// PTY (still streams PTY output to the browser).
    pub read_only: bool,
}

impl Config {
    #[must_use]
    pub const fn requires_auth(&self) -> bool {
        self.auth_secret.is_some()
    }
}

/// Internal shared state mounted on the router. Wraps [`Config`] with
/// values derived once at startup so per-request handlers don't
/// recompute them. The on-connect status frame text in particular is
/// constant over the lifetime of the server (it only depends on
/// `room_addr` and `read_only`), so encoding it on every WebSocket
/// upgrade is wasted JSON work.
#[derive(Debug)]
struct AppState {
    config: Config,
    /// JSON text of the on-connect [`StatusFrame`], serialized once at
    /// `router` build time. Serializing here is infallible — both
    /// fields are plain strings/bools — and the encoded bytes are
    /// reused verbatim on every WebSocket connect.
    status_frame_text: String,
}

impl AppState {
    fn new(config: Config) -> Self {
        let status_frame_text = encode_status_frame(&config);
        Self {
            config,
            status_frame_text,
        }
    }
}

fn encode_status_frame(config: &Config) -> String {
    let frame = StatusFrame {
        status: "connected",
        room: &config.room_addr.to_string(),
        read_only: config.read_only,
    };
    // Both fields serialize from owned String / bool; failure here is
    // a programming error (e.g. a future schema change), not a runtime
    // condition. Fall back to an empty object so a misconfigured frame
    // never crashes a healthy bridge.
    serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string())
}

/// Build the axum [`Router`] that backs ptyweb. Exposed so tests and
/// embedders can mount the bridge into a larger service tree.
pub fn router(config: Config) -> Router {
    let state = Arc::new(AppState::new(config));
    Router::new()
        .route("/", get(serve_index))
        .route("/viewer.js", get(serve_viewer_js))
        .route("/xterm.js", get(serve_xterm_js))
        .route("/xterm.css", get(serve_xterm_css))
        .route("/xterm-addon-fit.js", get(serve_xterm_addon_fit_js))
        .route("/healthz", get(serve_health))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

/// Run the ptyweb server until the listening socket errors.
///
/// # Errors
/// Binding the listener or running the HTTP service failed.
pub async fn serve(config: Config) -> Result<()> {
    let listen = config.listen_addr;
    let app = router(config);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind ptyweb listener on {listen}"))?;
    info!(addr = %listen, "ptyweb listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("ptyweb axum server stopped")?;
    Ok(())
}

async fn serve_index(State(state): State<Arc<AppState>>) -> Response {
    static_response(&state.config, "text/html; charset=utf-8", VIEWER_INDEX_HTML)
}

async fn serve_viewer_js(State(state): State<Arc<AppState>>) -> Response {
    static_response(
        &state.config,
        "application/javascript; charset=utf-8",
        VIEWER_JS,
    )
}

async fn serve_xterm_js(State(state): State<Arc<AppState>>) -> Response {
    static_response(
        &state.config,
        "application/javascript; charset=utf-8",
        XTERM_JS,
    )
}

async fn serve_xterm_css(State(state): State<Arc<AppState>>) -> Response {
    static_response(&state.config, "text/css; charset=utf-8", XTERM_CSS)
}

async fn serve_xterm_addon_fit_js(State(state): State<Arc<AppState>>) -> Response {
    static_response(
        &state.config,
        "application/javascript; charset=utf-8",
        XTERM_ADDON_FIT_JS,
    )
}

async fn serve_health() -> &'static str {
    "ok"
}

fn static_response(cfg: &Config, content_type: &'static str, body: &'static str) -> Response {
    let mut response = ([(header::CONTENT_TYPE, content_type)], body).into_response();
    apply_cors(cfg, response.headers_mut());
    response
}

fn apply_cors(cfg: &Config, headers: &mut HeaderMap) {
    let Some(origin) = cfg.allowed_origin.as_deref() else {
        return;
    };
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("X-PtyWeb-Auth, Content-Type"),
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeMsg {
    resize: ResizeBody,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeBody {
    cols: u16,
    rows: u16,
}

/// One-shot status frame emitted to the browser only after the
/// upstream ptyroom hello has been validated. The viewer uses it to
/// render a small badge (room address + read-only pill) and to refresh
/// that badge on every reconnect. Receiving this frame is the
/// browser's signal that the bridge is fully wired through to the
/// host — if upstream is unreachable the WebSocket closes with
/// status 1011 instead.
#[derive(Debug, Serialize)]
struct StatusFrame<'a> {
    status: &'static str,
    room: &'a str,
    read_only: bool,
}

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(status) = authorize(&state.config, &headers, peer) {
        warn!(%peer, status = status.as_u16(), "ptyweb auth rejected");
        return status.into_response();
    }
    let room_addr = state.config.room_addr;
    let read_only = state.config.read_only;
    // Hand the precomputed status frame to the per-connection bridge
    // by cloning the Arc, not the String — encoding ran once at
    // router-build time and the bytes are now reused for the lifetime
    // of the server.
    let app_state = state.clone();
    let mut response = ws.on_upgrade(move |socket| async move {
        if let Err(err) = bridge_socket(socket, room_addr, read_only, app_state).await {
            warn!(%peer, error = %err, "ptyweb bridge ended with error");
        } else {
            debug!(%peer, "ptyweb bridge closed");
        }
    });
    apply_cors(&state.config, response.headers_mut());
    response
}

/// Authorize a request. Returns the HTTP status to respond with on
/// rejection. `Ok(())` lets the request proceed.
///
/// Logic:
/// - If `--auth-secret` is set, the header must match.
/// - If `--auth-secret` is unset, only loopback peers are allowed.
fn authorize(cfg: &Config, headers: &HeaderMap, peer: SocketAddr) -> Result<(), StatusCode> {
    if let Some(expected) = cfg.auth_secret.as_deref() {
        let Some(presented) = headers.get(AUTH_HEADER).and_then(|v| v.to_str().ok()) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(())
    } else if peer.ip().is_loopback() {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// Constant-time byte comparison. Avoids leaking secret length or
/// content via timing side channels in the auth path.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn bridge_socket(
    mut socket: WebSocket,
    room_addr: SocketAddr,
    read_only: bool,
    state: Arc<AppState>,
) -> Result<()> {
    // Phase 1: upstream connect + hello handshake. Any failure here
    // closes the WebSocket with a status code so the browser can tell
    // a real "connected" event apart from a half-open bridge. The
    // status frame is *only* emitted after the server's hello has
    // been validated.
    let (handshaked, tcp_write) = match connect_and_handshake(room_addr).await {
        Ok(pair) => pair,
        Err(err) => {
            warn!(%room_addr, error = %err, "ptyweb upstream handshake failed");
            let _ = socket
                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    // 1011 = server error / unexpected condition.
                    code: 1011,
                    reason: "ptyroom upstream unavailable".into(),
                })))
                .await;
            return Err(err);
        }
    };

    // Channels carry messages from the WS reader to the TCP writer
    // and from the TCP reader to the WS writer. Splitting the WS lets
    // both halves make progress without lock contention.
    let (ws_tx_send, ws_tx_recv) = mpsc::channel::<WsOut>(CHANNEL_DEPTH);
    // `Bytes` lets browser-originated frames flow from axum
    // (Message::Binary already hands us Bytes) into the TCP writer
    // without a per-frame `to_vec()`.
    let (pty_tx_send, pty_tx_recv) = mpsc::channel::<Bytes>(CHANNEL_DEPTH);
    let (resize_tx, resize_rx) = mpsc::channel::<TerminalSize>(16);

    let (ws_sink, ws_stream) = socket.split();

    // Phase 2: now that upstream is fully ready, tell the browser.
    // Failure to enqueue is non-fatal — the channel is brand-new and
    // oversized. `status_frame_text` is precomputed at router-build
    // time; clone the String once per connect rather than re-running
    // `serde_json::to_string` against constant inputs.
    let _ = ws_tx_send
        .send(WsOut::Text(state.status_frame_text.clone()))
        .await;

    // Own all four loops in a single `JoinSet` so the bridge can both
    // detect the first exit *and* drive the rest to completion before
    // returning. Dropping a `JoinHandle` only detaches — it does not
    // release the resources held by the task's future. If `bridge_socket`
    // returned while tasks still ran they would keep the upstream
    // `TcpStream` halves and the `WebSocket` halves alive past the
    // visible end of the bridge, leaking sockets until the runtime
    // eventually polled them.
    //
    // Drop the bridge-local channel senders before joining so the
    // `*_loop` receivers observe channel closure and unwind naturally
    // once their peer exits. Without this, `tcp_writer_loop` and
    // `ws_writer_loop` would have to be aborted unconditionally instead
    // of finishing cleanly.
    let mut tasks: JoinSet<(&'static str, Result<()>)> = JoinSet::new();
    tasks.spawn(async move {
        let res = tcp_to_ws_loop(handshaked, ws_tx_send).await;
        ("tcp_to_ws", res)
    });
    tasks.spawn(async move {
        let res = tcp_writer_loop(tcp_write, pty_tx_recv, resize_rx).await;
        ("tcp_writer", res)
    });
    tasks.spawn(async move {
        let res = ws_writer_loop(ws_sink, ws_tx_recv).await;
        ("ws_writer", res)
    });
    tasks.spawn(async move {
        let res = ws_reader_loop(ws_stream, pty_tx_send, resize_tx, read_only).await;
        ("ws_reader", res)
    });

    // First task to exit ends the bridge. Abort the rest then drain the
    // join set so every task's resources are released before we return.
    let first = tasks.join_next().await;
    if let Some(res) = first {
        log_task_exit(res);
    }
    tasks.abort_all();
    while let Some(res) = tasks.join_next().await {
        log_task_exit(res);
    }
    Ok(())
}

fn log_task_exit(res: Result<(&'static str, Result<()>), tokio::task::JoinError>) {
    match res {
        Ok((label, Ok(()))) => debug!(task = label, "bridge task finished cleanly"),
        Ok((label, Err(err))) => debug!(task = label, error = %err, "bridge task ended"),
        Err(err) if err.is_cancelled() => debug!(error = %err, "bridge task cancelled"),
        Err(err) => debug!(error = %err, "bridge task panicked"),
    }
}

/// Frame to send over the WebSocket back to the browser.
///
/// `Binary` carries `Bytes` (not `Vec<u8>`) so the upstream
/// `ServerEvent::Output` payload — itself carved zero-copy from the
/// decoder's `BytesMut` — flows straight into `Message::Binary` without
/// an extra allocation.
enum WsOut {
    Binary(Bytes),
    Text(String),
}

/// Phase 1 of `bridge_socket`: TCP connect, send our hello, wait for
/// the server's hello and validate its protocol version. Any leftover
/// post-hello bytes from the initial read are preserved inside the
/// returned `ServerStream` so the main loop sees a continuous event
/// sequence with no gap.
async fn connect_and_handshake(
    room_addr: SocketAddr,
) -> Result<(HandshakedReader, tokio::net::tcp::OwnedWriteHalf)> {
    let tcp = TcpStream::connect(room_addr)
        .await
        .with_context(|| format!("connect ptyroom host at {room_addr}"))?;
    tcp.set_nodelay(true).ok();
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    tcp_write
        .write_all(&protocol::encode_hello_control())
        .await
        .context("send ptyroom hello")?;

    let mut stream = ServerStream::default();
    let mut pending_output: Vec<Bytes> = Vec::new();
    let mut buf = vec![0_u8; TCP_READ_BUF];
    let mut got_hello = false;
    while !got_hello {
        let n = tcp_read
            .read(&mut buf)
            .await
            .context("read ptyroom host hello")?;
        if n == 0 {
            return Err(anyhow::anyhow!(
                "ptyroom host closed connection before hello",
            ));
        }
        for event in stream.push(&buf[..n]) {
            // `_` arm covers future non_exhaustive ServerEvent variants.
            #[allow(clippy::collapsible_match, clippy::match_same_arms)]
            match event {
                ServerEvent::Hello(version) => {
                    if version != protocol::VERSION {
                        return Err(anyhow::anyhow!(
                            "unsupported ptyroom protocol version {version}; expected {}",
                            protocol::VERSION
                        ));
                    }
                    got_hello = true;
                }
                ServerEvent::Output(bytes) => pending_output.push(bytes),
                ServerEvent::Size(_) => {}
                _ => {}
            }
        }
    }

    Ok((
        HandshakedReader {
            tcp_read,
            stream,
            pending_output,
        },
        tcp_write,
    ))
}

/// Carries the TCP read half plus the partially-consumed
/// `ServerStream` from the handshake into the steady-state loop.
struct HandshakedReader {
    tcp_read: tokio::net::tcp::OwnedReadHalf,
    stream: ServerStream,
    pending_output: Vec<Bytes>,
}

async fn tcp_to_ws_loop(handshaked: HandshakedReader, ws_tx: mpsc::Sender<WsOut>) -> Result<()> {
    let HandshakedReader {
        mut tcp_read,
        mut stream,
        pending_output,
    } = handshaked;
    // Drain anything the handshake read past the server hello first
    // so the browser sees those bytes before any new TCP data.
    for bytes in pending_output {
        if ws_tx.send(WsOut::Binary(bytes)).await.is_err() {
            return Ok(());
        }
    }
    let mut buf = vec![0_u8; TCP_READ_BUF];
    loop {
        let n = tcp_read.read(&mut buf).await.context("read ptyroom host")?;
        if n == 0 {
            return Ok(());
        }
        for event in stream.push(&buf[..n]) {
            // `_` arm covers future non_exhaustive ServerEvent variants.
            #[allow(clippy::collapsible_match, clippy::match_same_arms)]
            match event {
                ServerEvent::Hello(version) => {
                    // A second hello on an already-handshaked stream
                    // is a protocol error; close the bridge.
                    return Err(anyhow::anyhow!(
                        "unexpected second ptyroom hello (version {version})"
                    ));
                }
                ServerEvent::Output(bytes) => {
                    if ws_tx.send(WsOut::Binary(bytes)).await.is_err() {
                        return Ok(());
                    }
                }
                ServerEvent::Size(_) => {
                    // Geometry advisories are informational for the
                    // browser. The viewer drives its own resize from
                    // window dimensions, so we don't forward here.
                }
                _ => {}
            }
        }
    }
}

async fn tcp_writer_loop(
    mut tcp_write: tokio::net::tcp::OwnedWriteHalf,
    mut pty_rx: mpsc::Receiver<Bytes>,
    mut resize_rx: mpsc::Receiver<TerminalSize>,
) -> Result<()> {
    loop {
        tokio::select! {
            maybe = pty_rx.recv() => {
                let Some(bytes) = maybe else { return Ok(()); };
                tcp_write.write_all(&bytes).await.context("write ptyroom host")?;
            }
            maybe = resize_rx.recv() => {
                let Some(size) = maybe else { return Ok(()); };
                let frame = protocol::encode_resize_control(size);
                tcp_write.write_all(&frame).await.context("write ptyroom resize")?;
            }
        }
    }
}

async fn ws_writer_loop(
    mut ws_sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<WsOut>,
) -> Result<()> {
    while let Some(out) = rx.recv().await {
        let msg = match out {
            // Message::Binary takes Bytes; no extra allocation needed.
            WsOut::Binary(bytes) => Message::Binary(bytes),
            WsOut::Text(text) => Message::Text(text.into()),
        };
        ws_sink.send(msg).await.context("ws send")?;
    }
    let _ = ws_sink.send(Message::Close(None)).await;
    Ok(())
}

async fn ws_reader_loop(
    mut ws_stream: futures_util::stream::SplitStream<WebSocket>,
    pty_tx: mpsc::Sender<Bytes>,
    resize_tx: mpsc::Sender<TerminalSize>,
    read_only: bool,
) -> Result<()> {
    while let Some(msg) = ws_stream.next().await {
        let msg = msg.context("ws recv")?;
        match msg {
            Message::Binary(bytes) => {
                if read_only {
                    continue;
                }
                // `Message::Binary` already holds a refcounted `Bytes`;
                // pass it straight through instead of `to_vec()`ing into
                // a fresh allocation.
                if pty_tx.send(bytes).await.is_err() {
                    return Ok(());
                }
            }
            Message::Text(text) => {
                if read_only {
                    continue;
                }
                match serde_json::from_str::<ResizeMsg>(&text) {
                    Ok(msg) => {
                        let size = TerminalSize::new(msg.resize.cols, msg.resize.rows);
                        if size.cols == 0 || size.rows == 0 {
                            continue;
                        }
                        // Match the cap enforced by `ptyrender::frame_replay`
                        // so a misbehaving browser can't drive the host PTY
                        // to an absurd grid size.
                        if size.cols > MAX_RESIZE_DIM || size.rows > MAX_RESIZE_DIM {
                            debug!(
                                cols = size.cols,
                                rows = size.rows,
                                max = MAX_RESIZE_DIM,
                                "ignoring oversize resize from browser"
                            );
                            continue;
                        }
                        if resize_tx.send(size).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(err) => {
                        debug!(error = %err, "ignoring malformed ws text frame");
                    }
                }
            }
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use ptyroom::protocol;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{AUTH_HEADER, Config, authorize, constant_time_eq};

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn config(auth: Option<&str>) -> Config {
        Config {
            room_addr: loopback(0),
            listen_addr: loopback(0),
            auth_secret: auth.map(str::to_owned),
            allowed_origin: None,
            read_only: false,
        }
    }

    #[test]
    fn constant_time_eq_matches_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn authorize_requires_header_when_secret_set() {
        let cfg = config(Some("topsecret"));
        let headers = HeaderMap::new();
        assert_eq!(
            authorize(&cfg, &headers, loopback(1234)).unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn authorize_rejects_wrong_header() {
        let cfg = config(Some("topsecret"));
        let mut headers = HeaderMap::new();
        headers.insert(AUTH_HEADER, HeaderValue::from_static("nope"));
        assert_eq!(
            authorize(&cfg, &headers, loopback(1234)).unwrap_err(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn authorize_accepts_matching_header() {
        let cfg = config(Some("topsecret"));
        let mut headers = HeaderMap::new();
        headers.insert(AUTH_HEADER, HeaderValue::from_static("topsecret"));
        authorize(&cfg, &headers, loopback(1234)).unwrap();
    }

    #[test]
    fn authorize_without_secret_allows_loopback() {
        let cfg = config(None);
        let headers = HeaderMap::new();
        authorize(&cfg, &headers, loopback(1234)).unwrap();
    }

    #[test]
    fn authorize_without_secret_blocks_non_loopback() {
        let cfg = config(None);
        let headers = HeaderMap::new();
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)), 8080);
        assert_eq!(
            authorize(&cfg, &headers, public).unwrap_err(),
            StatusCode::FORBIDDEN
        );
    }

    /// End-to-end check that ptyweb speaks the room protocol: spin up
    /// a mock host that emits a hello + a data frame, attach a
    /// `bridge_socket` to it, and assert the WS side sees the decoded
    /// payload bytes. We exercise the bridge with an in-memory
    /// WebSocket pair built via axum's testing extractor.
    #[tokio::test]
    async fn bridge_decodes_host_output_and_forwards_to_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let room_addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut hello = [0_u8; 32];
            let n = sock.read(&mut hello).await.unwrap();
            assert_eq!(&hello[..n], protocol::encode_hello_control().as_slice());
            sock.write_all(&protocol::encode_hello_control())
                .await
                .unwrap();
            sock.write_all(&protocol::encode_output_frame(b"hello browser"))
                .await
                .unwrap();
            // Hold the socket open briefly so the bridge can drain.
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        // Drive bridge_socket through an in-process WS pair. Easiest
        // path: spin up the full router on an ephemeral port, connect
        // a tungstenite client to /ws, observe the binary frame.
        let cfg = Config {
            room_addr,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            auth_secret: None,
            allowed_origin: None,
            read_only: false,
        };
        let app = super::router(cfg);
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let serve_task = tokio::spawn(async move {
            axum::serve(
                ws_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let received = bridge_smoke_collect(ws_addr).await;
        assert!(
            received
                .windows(b"hello browser".len())
                .any(|w| w == b"hello browser"),
            "expected payload in {received:?}",
        );

        serve_task.abort();
        let _ = server.await;
    }

    /// Connect-time JSON status frame announces the room and the
    /// read-only flag so the viewer can render its badge without
    /// guessing at the URL.
    #[tokio::test]
    async fn bridge_emits_status_frame_on_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let room_addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut hello = [0_u8; 32];
            let _ = sock.read(&mut hello).await.unwrap();
            sock.write_all(&protocol::encode_hello_control())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let cfg = Config {
            room_addr,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            auth_secret: None,
            allowed_origin: None,
            read_only: true,
        };
        let app = super::router(cfg);
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let serve_task = tokio::spawn(async move {
            axum::serve(
                ws_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let (opcode, payload) = read_one_frame(ws_addr).await;
        assert_eq!(opcode, 0x1, "expected text frame, got opcode {opcode}");
        let text = std::str::from_utf8(&payload).unwrap();
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(json["status"], "connected");
        assert_eq!(json["room"], room_addr.to_string());
        assert_eq!(json["read_only"], true);

        serve_task.abort();
        let _ = server.await;
    }

    /// Minimal raw-WebSocket client sufficient to read framed
    /// payloads ptyweb sends. Returns `(opcode, payload)` for one
    /// server → client frame. Avoids pulling in a full WS client dep
    /// just for one assertion.
    async fn read_one_frame(addr: SocketAddr) -> (u8, Vec<u8>) {
        use tokio::net::TcpStream;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        complete_handshake(&mut stream, addr).await;
        read_frame(&mut stream).await
    }

    async fn complete_handshake(stream: &mut tokio::net::TcpStream, addr: SocketAddr) {
        let key = "dGhlIHNhbXBsZSBub25jZQ=="; // RFC 6455 example key
        let req = format!(
            "GET /ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut headers = Vec::new();
        let mut byte = [0_u8; 1];
        while !headers.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut byte).await.unwrap();
            assert!(n != 0, "server closed before handshake completed");
            headers.push(byte[0]);
        }
    }

    async fn read_frame(stream: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
        let mut hdr = [0_u8; 2];
        stream.read_exact(&mut hdr).await.unwrap();
        let opcode = hdr[0] & 0x0F;
        let len = usize::from(hdr[1] & 0x7F);
        let len = if len == 126 {
            let mut ext = [0_u8; 2];
            stream.read_exact(&mut ext).await.unwrap();
            usize::from(u16::from_be_bytes(ext))
        } else if len == 127 {
            let mut ext = [0_u8; 8];
            stream.read_exact(&mut ext).await.unwrap();
            usize::try_from(u64::from_be_bytes(ext)).unwrap()
        } else {
            len
        };
        let mut payload = vec![0_u8; len];
        stream.read_exact(&mut payload).await.unwrap();
        (opcode, payload)
    }

    /// Collect frames until a binary one arrives, skipping the
    /// connect-time status text frame and anything else advisory.
    async fn bridge_smoke_collect(addr: SocketAddr) -> Vec<u8> {
        use tokio::net::TcpStream;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        complete_handshake(&mut stream, addr).await;
        for _ in 0..8 {
            let (opcode, payload) = read_frame(&mut stream).await;
            match opcode {
                0x2 => return payload,
                0x1 => {} // text frame — status / advisory; skip
                other => panic!("unexpected opcode {other}"),
            }
        }
        panic!("no binary frame within 8 reads");
    }

    /// Smoke test that `bridge_socket` returns when the host hangs up
    /// without sending hello (we want a clean error, not a panic).
    #[tokio::test]
    async fn bridge_errors_when_host_disconnects_before_hello() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let room_addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain hello then drop.
            let mut buf = [0_u8; 64];
            let _ = sock.read(&mut buf).await;
        });

        // We can't easily synthesize a WebSocket without going through
        // the HTTP upgrade, so we drive `tcp_to_ws_loop` directly via
        // bridge_socket and let the channel sender close.
        let cfg = Config {
            room_addr,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            auth_secret: None,
            allowed_origin: None,
            read_only: false,
        };
        // bridge_socket needs a WebSocket. The cheap proxy: use the
        // router approach again, but the server-side closes
        // immediately so the bridge task should terminate.
        let app = super::router(cfg);
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let serve_task = tokio::spawn(async move {
            axum::serve(
                ws_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        // Open + immediately drop the WS to force the bridge to wind down.
        let mut stream = tokio::net::TcpStream::connect(ws_addr).await.unwrap();
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let req = format!(
            "GET /ws HTTP/1.1\r\nHost: {ws_addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        // Read headers then drop.
        let mut buf = [0_u8; 256];
        let _ = stream.read(&mut buf).await;
        drop(stream);

        tokio::time::sleep(Duration::from_millis(50)).await;
        serve_task.abort();
        let _ = server.await;
    }

    /// Regression for the `JoinSet` cleanup fix: when the browser-side
    /// WebSocket drops, the upstream TCP connection to the ptyroom host
    /// must close promptly. Before the fix, `bridge_socket` returned as
    /// soon as one of the four loop tasks exited; the rest were
    /// detached and kept the TCP halves alive, so the host saw the
    /// socket as still connected long after the browser left.
    ///
    /// The mock host echoes a hello then reads from the bridge in a
    /// loop, recording whether the read terminated with `Ok(0)` (clean
    /// EOF after our bridge dropped the socket) within a short window.
    #[tokio::test]
    async fn bridge_releases_upstream_tcp_when_browser_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let room_addr = listener.local_addr().unwrap();
        let (eof_tx, mut eof_rx) = tokio::sync::oneshot::channel::<bool>();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut hello = [0_u8; 32];
            let n = sock.read(&mut hello).await.unwrap();
            assert_eq!(&hello[..n], protocol::encode_hello_control().as_slice());
            sock.write_all(&protocol::encode_hello_control())
                .await
                .unwrap();
            // Drain until the bridge drops its write half. With the bug
            // present, the bridge keeps the TCP halves alive past
            // browser disconnect and this read blocks forever.
            let mut buf = [0_u8; 1024];
            let saw_eof = loop {
                match sock.read(&mut buf).await {
                    Ok(0) => break true,
                    Ok(_) => {}
                    Err(_) => break false,
                }
            };
            let _ = eof_tx.send(saw_eof);
        });

        let cfg = Config {
            room_addr,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            auth_secret: None,
            allowed_origin: None,
            read_only: false,
        };
        let app = super::router(cfg);
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let serve_task = tokio::spawn(async move {
            axum::serve(
                ws_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        // Open browser WS, wait for the status frame so we know the
        // bridge is fully wired through, then drop the socket. The
        // bridge must abort and release the upstream TCP halves.
        let mut stream = tokio::net::TcpStream::connect(ws_addr).await.unwrap();
        complete_handshake(&mut stream, ws_addr).await;
        let (opcode, _payload) = read_frame(&mut stream).await;
        assert_eq!(opcode, 0x1, "expected status text frame");
        drop(stream);

        // Wait up to a second for the mock host to observe EOF. With
        // the bug, this times out because the detached tasks keep the
        // upstream socket alive past `bridge_socket` return.
        let saw_eof = tokio::time::timeout(Duration::from_secs(1), &mut eof_rx)
            .await
            .expect("upstream TCP never closed after browser disconnect")
            .expect("mock host channel dropped");
        assert!(saw_eof, "mock host did not observe a clean EOF");

        serve_task.abort();
        let _ = server.await;
    }
}
