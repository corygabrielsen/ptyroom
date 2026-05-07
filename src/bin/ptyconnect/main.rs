//! `ptyconnect` CLI: attach to a `ptyshare` session.

use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "ptyconnect — connect your terminal to a ptyshare session",
    long_about = "Connect stdin/stdout to a `ptyshare` TCP session. The transport\n\
                  reports local terminal resizes so the shared PTY can use a\n\
                  size every attached client can display. Interactive clients\n\
                  render the shared screen in a local alternate screen. The transport\n\
                  has no built-in auth or encryption; connect through SSH,\n\
                  WireGuard, or another trusted tunnel outside loopback."
)]
struct Args {
    /// ptyshare host:port.
    addr: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ptytrace::pty::connect::connect(args.addr)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Args;

    #[test]
    fn parses_session_addr() {
        let args = Args::try_parse_from(["ptyconnect", "127.0.0.1:7000"]).unwrap();

        assert_eq!(args.addr.port(), 7000);
    }

    #[test]
    fn help_warns_about_transport_security() {
        let help = Args::command().render_long_help().to_string();

        assert!(help.contains("no built-in auth"));
        assert!(help.contains("trusted tunnel"));
    }
}
