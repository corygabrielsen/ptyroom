//! `ptyroom` CLI: high-level shared terminal rooms.

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use clap::{Args as ClapArgs, Parser, Subcommand};
use ptyroom::connect;
use ptyroom::share::{ShareOpts, host_local_io_notice, run};

#[derive(Parser)]
#[command(
    version,
    about = "ptyroom — open, join, or watch a shared terminal room",
    long_about = "Open, join, or watch a shared terminal room. The host terminal is\n\
                  connected by default. The room transport has no built-in auth\n\
                  or encryption; bind loopback and use SSH,\n\
                  WireGuard, or another trusted tunnel for remote use.\n\n\
                  Interactive join controls are local: press Ctrl-] then . to\n\
                  detach, ? for help, r to redraw, or Ctrl-] to send a literal\n\
                  Ctrl-]. Watch clients are read-only and do not affect the\n\
                  shared PTY size."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Host a shared terminal room.
    Host(HostArgs),
    /// Join an existing room.
    Join(JoinArgs),
    /// Watch an existing room without sending input or resizing it.
    Watch(JoinArgs),
}

#[derive(ClapArgs)]
struct HostArgs {
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
    /// Allow binding a no-auth/no-encryption room outside loopback.
    #[arg(long)]
    allow_unauthenticated_public_bind: bool,
    /// Command to run in the room. Empty uses `$SHELL` or `bash`.
    #[arg(
        value_name = "COMMAND",
        num_args = 0..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

#[derive(ClapArgs, Clone, Copy)]
struct JoinArgs {
    /// Room host:port printed by `ptyroom host`.
    addr: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Host(args) => host(args),
        Command::Join(args) => join(args),
        Command::Watch(args) => watch(args),
    }
}

fn host(args: HostArgs) -> anyhow::Result<()> {
    validate_listen_addr(args.listen, args.allow_unauthenticated_public_bind)?;
    validate_terminal_size(args.cols, args.rows)?;
    let listener = TcpListener::bind(args.listen)?;
    let bound_addr = listener.local_addr()?;
    eprintln!("[ptyroom listening on {bound_addr}]");
    eprintln!("[join with: ptyroom join {bound_addr}]");
    eprintln!("[watch with: ptyroom watch {bound_addr}]");
    if args.allow_unauthenticated_public_bind && !bound_addr.ip().is_loopback() {
        eprintln!("[warning: unauthenticated public ptyroom bind]");
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
    println!(
        "render with: ptyrender {} room.gif",
        summary.trace_path.display()
    );
    Ok(())
}

fn join(args: JoinArgs) -> anyhow::Result<()> {
    connect::connect(args.addr)
}

fn watch(args: JoinArgs) -> anyhow::Result<()> {
    connect::watch(args.addr)
}

fn default_trace_path(addr: SocketAddr) -> PathBuf {
    PathBuf::from(format!("ptyroom-{}-{}.ptytrace", addr.ip(), addr.port()))
}

fn validate_terminal_size(cols: u16, rows: u16) -> anyhow::Result<()> {
    if cols == 0 || rows == 0 {
        return Err(anyhow!(
            "ptyroom terminal size must be nonzero; got {cols}x{rows}"
        ));
    }
    Ok(())
}

fn validate_listen_addr(addr: SocketAddr, allow_public: bool) -> anyhow::Result<()> {
    if addr.ip().is_loopback() || allow_public {
        return Ok(());
    }
    Err(anyhow!(
        "refusing to bind unauthenticated ptyroom to {addr}; \
         use --allow-unauthenticated-public-bind only behind a trusted network boundary"
    ))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, default_trace_path, validate_listen_addr, validate_terminal_size};

    #[test]
    fn parses_host_command_after_options() {
        let cli = Cli::try_parse_from([
            "ptyroom",
            "host",
            "--listen",
            "127.0.0.1:7000",
            "bash",
            "-l",
        ])
        .unwrap();

        let Command::Host(args) = cli.command else {
            panic!("expected host subcommand");
        };
        assert_eq!(args.command, ["bash", "-l"]);
        assert_eq!(args.listen.port(), 7000);
    }

    #[test]
    fn parses_join_addr() {
        let cli = Cli::try_parse_from(["ptyroom", "join", "127.0.0.1:7000"]).unwrap();

        let Command::Join(args) = cli.command else {
            panic!("expected join subcommand");
        };
        assert_eq!(args.addr.port(), 7000);
    }

    #[test]
    fn parses_watch_addr() {
        let cli = Cli::try_parse_from(["ptyroom", "watch", "127.0.0.1:7000"]).unwrap();

        let Command::Watch(args) = cli.command else {
            panic!("expected watch subcommand");
        };
        assert_eq!(args.addr.port(), 7000);
    }

    #[test]
    fn default_trace_path_uses_ptyroom_name() {
        let path = default_trace_path(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8022));

        assert_eq!(path.file_name().unwrap(), "ptyroom-127.0.0.1-8022.ptytrace");
    }

    #[test]
    fn rejects_public_bind_without_explicit_allow() {
        let addr = "0.0.0.0:7000".parse().unwrap();

        let err = validate_listen_addr(addr, false).unwrap_err().to_string();

        assert!(err.contains("refusing to bind"));
        assert!(validate_listen_addr(addr, true).is_ok());
    }

    #[test]
    fn rejects_zero_terminal_dimensions() {
        let err = validate_terminal_size(0, 24).unwrap_err().to_string();

        assert!(err.contains("nonzero"));
        assert!(validate_terminal_size(80, 24).is_ok());
    }

    #[test]
    fn help_mentions_host_and_join() {
        let help = Cli::command().render_long_help().to_string();

        assert!(help.contains("host"));
        assert!(help.contains("join"));
        assert!(help.contains("watch"));
        assert!(help.contains("no built-in auth"));
        assert!(help.contains("read-only"));
        assert!(help.contains("Ctrl-] then ."));
    }
}
