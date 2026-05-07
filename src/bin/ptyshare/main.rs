//! `ptyshare` CLI: host a shared, recorded PTY session.

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use clap::Parser;
use ptytrace::pty::share::{ShareOpts, host_local_io_notice, run};

#[derive(Parser)]
#[command(
    version,
    about = "ptyshare — host a collaborative recorded PTY session",
    long_about = "Host a command under a PTY, accept TCP clients, interleave client\n\
                  input into the PTY, broadcast output to every client, and write\n\
                  a `.ptytrace` recording. The shared PTY is resized to the\n\
                  smallest known attached rendering terminal. The host terminal is also\n\
                  connected by default. This transport has no built-in auth or encryption;\n\
                  bind loopback and use SSH/WireGuard for remote use."
)]
struct Args {
    /// Address to listen on. Defaults to loopback with an OS-assigned port.
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: SocketAddr,
    /// Output trace path.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Terminal columns.
    #[arg(long, default_value_t = 80)]
    cols: u16,
    /// Terminal rows.
    #[arg(long, default_value_t = 24)]
    rows: u16,
    /// Maximum session duration in seconds.
    #[arg(long, default_value_t = 3600)]
    max_secs: u64,
    /// Do not tee PTY output to the host's stdout.
    #[arg(long)]
    no_local_output: bool,
    /// Do not forward the host's stdin into the PTY.
    #[arg(long)]
    no_local_input: bool,
    /// Allow binding a no-auth/no-encryption session outside loopback.
    #[arg(long)]
    allow_unauthenticated_public_bind: bool,
    /// Command to run under the shared PTY. Empty uses `$SHELL` or `bash`.
    #[arg(
        value_name = "COMMAND",
        num_args = 0..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    validate_listen_addr(args.listen, args.allow_unauthenticated_public_bind)?;
    let listener = TcpListener::bind(args.listen)?;
    let bound_addr = listener.local_addr()?;
    eprintln!("[ptyshare listening on {bound_addr}]");
    eprintln!("[connect with: ptyconnect {bound_addr}]");
    if args.allow_unauthenticated_public_bind && !bound_addr.ip().is_loopback() {
        eprintln!("[warning: unauthenticated public ptyshare bind]");
    }
    if let Some(notice) = host_local_io_notice(!args.no_local_input, !args.no_local_output) {
        eprintln!("{notice}");
    }
    let out = args.out.unwrap_or_else(|| default_trace_path(bound_addr));
    let summary = run(
        &listener,
        ShareOpts {
            argv: args.command,
            cols: args.cols,
            rows: args.rows,
            out,
            max_runtime: Duration::from_secs(args.max_secs),
            local_output: !args.no_local_output,
            local_input: !args.no_local_input,
        },
    )?;
    println!(
        "wrote {} ({} events, {} client(s), {} disconnect(s), {} backlog drop(s))",
        summary.trace_path.display(),
        summary.events,
        summary.clients_accepted,
        summary.clients_disconnected,
        summary.clients_dropped_for_backlog
    );
    Ok(())
}

fn default_trace_path(addr: SocketAddr) -> PathBuf {
    PathBuf::from(format!("ptyshare-{}-{}.ptytrace", addr.ip(), addr.port()))
}

fn validate_listen_addr(addr: SocketAddr, allow_public: bool) -> anyhow::Result<()> {
    if addr.ip().is_loopback() || allow_public {
        return Ok(());
    }
    Err(anyhow!(
        "refusing to bind unauthenticated ptyshare session to {addr}; \
         use --allow-unauthenticated-public-bind only behind a trusted network boundary"
    ))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use clap::{CommandFactory, Parser};

    use super::{Args, default_trace_path, validate_listen_addr};

    #[test]
    fn default_trace_path_mentions_bound_port() {
        let path = default_trace_path(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8022));
        assert_eq!(
            path.file_name().unwrap(),
            "ptyshare-127.0.0.1-8022.ptytrace"
        );
    }

    #[test]
    fn parses_command_after_options() {
        let args =
            Args::try_parse_from(["ptyshare", "--listen", "127.0.0.1:7000", "bash", "-l"]).unwrap();

        assert_eq!(args.command, ["bash", "-l"]);
        assert_eq!(args.listen.port(), 7000);
    }

    #[test]
    fn parses_local_input_and_public_bind_flags() {
        let args = Args::try_parse_from([
            "ptyshare",
            "--no-local-input",
            "--allow-unauthenticated-public-bind",
            "--listen",
            "0.0.0.0:7000",
        ])
        .unwrap();

        assert!(args.no_local_input);
        assert!(args.allow_unauthenticated_public_bind);
    }

    #[test]
    fn rejects_public_bind_without_explicit_allow() {
        let addr = "0.0.0.0:7000".parse().unwrap();

        let err = validate_listen_addr(addr, false).unwrap_err().to_string();

        assert!(err.contains("refusing to bind"));
        assert!(validate_listen_addr(addr, true).is_ok());
    }

    #[test]
    fn help_warns_about_transport_security() {
        let help = Args::command().render_long_help().to_string();

        assert!(help.contains("no built-in auth"));
        assert!(help.contains("SSH/WireGuard"));
    }
}
