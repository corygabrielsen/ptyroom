//! `termroom` CLI: demo-facing shared terminal rooms.

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::{Args as ClapArgs, Parser, Subcommand};
use ptytrace::pty::share::{ShareOpts, run};

#[derive(Parser)]
#[command(
    version,
    about = "termroom — open or join a shared terminal room",
    long_about = "Open a shared terminal room, join one, or run a curated local\n\
                  demo. `termroom` is the high-level facade over the lower-level\n\
                  `ptyshare` and `ptyconnect` transport tools. The transport has\n\
                  no built-in auth or encryption; bind loopback and use SSH,\n\
                  WireGuard, or another trusted tunnel for remote use."
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
    /// Host a local demo room with practical defaults.
    Demo(DemoArgs),
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
    /// Room host:port printed by `termroom host`.
    addr: SocketAddr,
}

#[derive(ClapArgs)]
struct DemoArgs {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:7373")]
    listen: SocketAddr,
    /// Output trace path.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Terminal columns.
    #[arg(long, default_value_t = 100)]
    cols: u16,
    /// Terminal rows.
    #[arg(long, default_value_t = 30)]
    rows: u16,
    /// Maximum session duration in seconds.
    #[arg(long, default_value_t = 3600)]
    max_secs: u64,
    /// Command to run in the room. Empty opens an interactive shell.
    #[arg(
        value_name = "COMMAND",
        num_args = 0..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Host(args) => host(args),
        Command::Join(args) => join(args),
        Command::Demo(args) => demo(args),
    }
}

fn demo(args: DemoArgs) -> anyhow::Result<()> {
    host(HostArgs {
        listen: args.listen,
        out: Some(
            args.out
                .unwrap_or_else(|| PathBuf::from("/tmp/termroom-demo.ptytrace")),
        ),
        cols: args.cols,
        rows: args.rows,
        max_secs: args.max_secs,
        no_local_output: false,
        no_local_input: false,
        allow_unauthenticated_public_bind: false,
        command: if args.command.is_empty() {
            demo_command()
        } else {
            args.command
        },
    })
}

fn host(args: HostArgs) -> anyhow::Result<()> {
    validate_listen_addr(args.listen, args.allow_unauthenticated_public_bind)?;
    let listener = TcpListener::bind(args.listen)?;
    let bound_addr = listener.local_addr()?;
    eprintln!("[termroom listening on {bound_addr}]");
    eprintln!("[join with: termroom join {bound_addr}]");
    if args.allow_unauthenticated_public_bind && !bound_addr.ip().is_loopback() {
        eprintln!("[warning: unauthenticated public termroom bind]");
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

fn join(args: JoinArgs) -> anyhow::Result<()> {
    let ptyconnect = sibling_binary("ptyconnect")?;
    let status = ProcessCommand::new(&ptyconnect)
        .arg(args.addr.to_string())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("run {}", ptyconnect.display()))?;
    if !status.success() {
        anyhow::bail!("ptyconnect exited with {status}");
    }
    Ok(())
}

fn sibling_binary(name: &str) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("locate current executable")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?;
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if path.exists() {
        return Ok(path);
    }
    Err(anyhow!(
        "could not find sibling binary {}; run `cargo build --bins` or install all ptytrace binaries",
        path.display()
    ))
}

fn demo_command() -> Vec<String> {
    vec![
        "sh".into(),
        "-lc".into(),
        "printf 'termroom demo room is open\\n'; \
         printf 'type here; peers join with the command above\\n'; \
         exec \"${SHELL:-sh}\""
            .into(),
    ]
}

fn default_trace_path(addr: SocketAddr) -> PathBuf {
    PathBuf::from(format!("termroom-{}-{}.ptytrace", addr.ip(), addr.port()))
}

fn validate_listen_addr(addr: SocketAddr, allow_public: bool) -> anyhow::Result<()> {
    if addr.ip().is_loopback() || allow_public {
        return Ok(());
    }
    Err(anyhow!(
        "refusing to bind unauthenticated termroom to {addr}; \
         use --allow-unauthenticated-public-bind only behind a trusted network boundary"
    ))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, default_trace_path, demo_command, validate_listen_addr};

    #[test]
    fn parses_host_command_after_options() {
        let cli = Cli::try_parse_from([
            "termroom",
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
        let cli = Cli::try_parse_from(["termroom", "join", "127.0.0.1:7000"]).unwrap();

        let Command::Join(args) = cli.command else {
            panic!("expected join subcommand");
        };
        assert_eq!(args.addr.port(), 7000);
    }

    #[test]
    fn demo_has_shell_command_default() {
        let command = demo_command();

        assert_eq!(command[0], "sh");
        assert!(command[2].contains("termroom demo room is open"));
    }

    #[test]
    fn default_trace_path_uses_termroom_name() {
        let path = default_trace_path(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8022));

        assert_eq!(
            path.file_name().unwrap(),
            "termroom-127.0.0.1-8022.ptytrace"
        );
    }

    #[test]
    fn rejects_public_bind_without_explicit_allow() {
        let addr = "0.0.0.0:7000".parse().unwrap();

        let err = validate_listen_addr(addr, false).unwrap_err().to_string();

        assert!(err.contains("refusing to bind"));
        assert!(validate_listen_addr(addr, true).is_ok());
    }

    #[test]
    fn help_mentions_host_and_join() {
        let help = Cli::command().render_long_help().to_string();

        assert!(help.contains("host"));
        assert!(help.contains("join"));
        assert!(help.contains("no built-in auth"));
    }
}
