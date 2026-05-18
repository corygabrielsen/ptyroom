//! `ptyweb` CLI entry point. Parses flags, builds [`ptyweb::Config`],
//! and runs the server until the process is signalled.

use std::net::SocketAddr;

use anyhow::Result;
use clap::Parser;
use ptyweb::Config;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Browser ↔ ptyroom WebSocket bridge.",
    long_about = "Bridge one ptyroom host to one WebSocket listener.\n\
                  Browsers attach to /ws and receive PTY output bytes as binary\n\
                  WebSocket frames. Keystrokes go back as binary frames; resize\n\
                  events as JSON text frames.\n\n\
                  Production deployments sit behind a reverse proxy that\n\
                  terminates TLS and injects the X-PtyWeb-Auth header. Without\n\
                  --auth-secret, ptyweb refuses non-loopback connections."
)]
struct Cli {
    /// TCP address of the ptyroom host to bridge (e.g. `127.0.0.1:7373`).
    #[arg(long, value_name = "ADDR")]
    room: SocketAddr,
    /// Address ptyweb listens on for WebSocket clients
    /// (e.g. `127.0.0.1:8001`).
    #[arg(long, value_name = "ADDR")]
    listen: SocketAddr,
    /// Shared secret the reverse proxy injects via `X-PtyWeb-Auth`.
    /// When omitted, ptyweb refuses non-loopback connections.
    #[arg(long, value_name = "STRING", env = "PTYWEB_AUTH_SECRET")]
    auth_secret: Option<String>,
    /// Value for `Access-Control-Allow-Origin`. When omitted, no
    /// CORS header is set.
    #[arg(long, value_name = "ORIGIN")]
    allowed_origin: Option<String>,
    /// Refuse to forward browser-originated bytes to the PTY.
    #[arg(long)]
    read_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let config = Config {
        room_addr: cli.room,
        listen_addr: cli.listen,
        auth_secret: cli.auth_secret,
        allowed_origin: cli.allowed_origin,
        read_only: cli.read_only,
    };
    ptyweb::serve(config).await
}
