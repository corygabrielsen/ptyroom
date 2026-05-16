//! `ptyroom` CLI: high-level shared terminal rooms.

use std::ffi::OsStr;
use std::io::{IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Print one `wrote PATH` line. Honors the invariants documented
/// in `../ptyrecord/INVARIANTS.md`. See the matching `print_wrote`
/// in `ptyrecord/src/main.rs` for the rationale; this is the same
/// pattern, applied here so `ptyroom host`'s post-session output
/// shares the contract.
fn print_wrote(path: impl std::fmt::Display) {
    if std::io::stdout().is_terminal() {
        print!("\x1b[2K\r");
    }
    println!("wrote {path}");
}

use anyhow::{Context, anyhow};
use clap::{Args as ClapArgs, Parser, Subcommand};
use ptyrecord::PtyRecord;
use ptyrender::witness::{RenderOptions, Witness};
use ptyroom::connect;
use ptyroom::share::{ShareOpts, ctl_socket_path, host_local_io_notice, run};
use tempfile::TempDir;

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
    /// Send a control command to a running ptyroom host (queue management).
    Ctl(CtlArgs),
}

#[derive(ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
struct HostArgs {
    /// Address to listen on. Defaults to loopback with an OS-assigned port.
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: SocketAddr,
    /// Output `.ptyrecord` bundle path. Defaults to
    /// `ptyroom-<ip>-<port>.ptyrecord` in the current directory.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Optional sidecar copy of the raw `.ptytrace`. By default the
    /// trace is bundled inside the `.ptyrecord` and the mid-session
    /// scratch file is deleted on clean shutdown.
    #[arg(long)]
    trace_out: Option<PathBuf>,
    /// Optional sidecar copy of the rendered MP4 media.
    #[arg(long, conflicts_with = "bundle_only")]
    media_out: Option<PathBuf>,
    /// Optional sidecar copy of the witness JSON embedded in the bundle.
    #[arg(long, conflicts_with = "no_witness")]
    witness_out: Option<PathBuf>,
    /// Do not embed a reproducibility witness.
    #[arg(long)]
    no_witness: bool,
    /// Suppress the default `<stem>.mp4` sidecar; write only the
    /// `.ptyrecord` bundle.
    #[arg(long)]
    bundle_only: bool,
    /// Font size in pixels for the rendered media.
    #[arg(long, default_value_t = 14.0)]
    font_size: f32,
    /// Padding around the rendered grid in pixels.
    #[arg(long, default_value_t = 12)]
    padding: u32,
    /// Optional output width in pixels (lanczos scaling).
    #[arg(long)]
    width: Option<u32>,
    /// Output frame rate.
    #[arg(long, default_value_t = 25)]
    fps: u32,
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

#[derive(ClapArgs)]
struct CtlArgs {
    /// Room host:port printed by `ptyroom host`. Used to locate the
    /// host's local control socket (`/tmp/ptyroom-<port>.sock`).
    addr: SocketAddr,
    /// Control namespace.
    #[command(subcommand)]
    namespace: CtlNamespace,
}

#[derive(Subcommand)]
enum CtlNamespace {
    /// Queue operations: enqueue messages and inject them into the PTY.
    Queue {
        #[command(subcommand)]
        op: CtlQueueOp,
    },
}

#[derive(Subcommand)]
enum CtlQueueOp {
    /// Append a message to the host's queue. Reads stdin when no text is given.
    Add {
        /// Message text. If omitted, the text is read from stdin until EOF.
        text: Option<String>,
    },
    /// Inject the next queued message into the shared PTY, followed by Enter.
    Next,
    /// Print the current queue depth.
    List,
    /// Empty the queue without injecting anything.
    Clear,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Host(args) => host(args),
        Command::Join(args) => join(args),
        Command::Watch(args) => watch(args),
        Command::Ctl(args) => ctl(args),
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

    let out = args.out.unwrap_or_else(|| default_bundle_path(bound_addr));
    let stem = bundle_stem(&out);

    let work = TempDir::new()?;
    // Trace is written incrementally during the session. When the
    // user passed `--trace-out`, write to that user-visible path so
    // they get a durable copy. Otherwise route through the TempDir;
    // bundle building reads it back at session end before `work`
    // drops, and a SIGKILL/SIGSEGV mid-session leaves the trace in
    // /tmp until next OS cleanup. Matches ptyrecord's default model.
    let trace_path = args
        .trace_out
        .clone()
        .unwrap_or_else(|| work.path().join(format!("{stem}.ptytrace")));
    let trace_is_sidecar = args.trace_out.is_some();
    let media_path = match (&args.media_out, args.bundle_only) {
        (Some(p), _) => p.clone(),
        (None, false) => out.with_extension("mp4"),
        (None, true) => work.path().join(format!("{stem}.mp4")),
    };
    let media_is_sidecar = args.media_out.is_some() || !args.bundle_only;
    ensure_mp4_path(&media_path)?;
    ensure_parent(&out)?;
    ensure_parent(&trace_path)?;
    ensure_parent(&media_path)?;

    let summary = run(
        &listener,
        ShareOpts {
            argv: args.command,
            cols: args.cols,
            rows: args.rows,
            out: trace_path.clone(),
            max_runtime: Duration::from_secs(args.max_secs),
            local_output: !args.no_local_output,
            local_input: !args.no_local_input,
        },
    )?;
    eprintln!(
        "[session ended: {} events, {} client(s), {} disconnect(s), {} backlog drop(s)]",
        summary.events,
        summary.clients_accepted,
        summary.clients_disconnected,
        summary.clients_dropped_for_backlog
    );

    // No-output session = no encodable frames. The encoder would error
    // with "timing has no frames". Skip media + bundle entirely; the
    // trace itself is fine to keep around as evidence of the empty
    // session. Always keep it even if the user didn't pass
    // `--trace-out`, since otherwise we'd produce zero artifacts.
    if summary.events == 0 {
        eprintln!("[no output events captured; skipping render + bundle]");
        print_wrote(trace_path.display());
        return Ok(());
    }

    eprintln!("[rendering media → {}…]", media_path.display());

    let mut render = ptyrender::render(&trace_path)
        .context("load trace for render")?
        .font_size(args.font_size)
        .padding(args.padding)
        .fps(args.fps);
    if let Some(w) = args.width {
        render = render.width(w);
    }
    render
        .to_path(&media_path)
        .context("render trace to media")?;

    let witness = (!args.no_witness)
        .then(|| {
            Witness::from_rendered_output(
                &trace_path,
                &media_path,
                RenderOptions::libx264(args.font_size, args.padding, args.width, args.fps),
            )
        })
        .transpose()?;
    if let (Some(witness), Some(witness_out)) = (&witness, &args.witness_out) {
        ensure_parent(witness_out)?;
        witness.write(witness_out)?;
    }

    let record = PtyRecord::from_paths(&trace_path, &media_path, witness.as_ref())?;
    record.write(&out)?;

    print_wrote(out.display());
    if media_is_sidecar {
        print_wrote(media_path.display());
    }
    if trace_is_sidecar {
        print_wrote(trace_path.display());
    }
    if let Some(witness_out) = &args.witness_out {
        print_wrote(witness_out.display());
    }

    Ok(())
}

fn join(args: JoinArgs) -> anyhow::Result<()> {
    connect::connect(args.addr)
}

fn watch(args: JoinArgs) -> anyhow::Result<()> {
    connect::watch(args.addr)
}

fn ctl(args: CtlArgs) -> anyhow::Result<()> {
    let socket_path = ctl_socket_path(args.addr.port());
    let mut stream = UnixStream::connect(&socket_path).with_context(|| {
        format!(
            "connect ptyroom control socket at {}",
            socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let CtlNamespace::Queue { op } = args.namespace;
    match op {
        CtlQueueOp::Add { text } => {
            let payload = if let Some(t) = text {
                t
            } else {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            };
            let header = format!("add {}\n", payload.len());
            stream.write_all(header.as_bytes())?;
            stream.write_all(payload.as_bytes())?;
        }
        CtlQueueOp::Next => stream.write_all(b"next\n")?,
        CtlQueueOp::List => stream.write_all(b"list\n")?,
        CtlQueueOp::Clear => stream.write_all(b"clear\n")?,
    }
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let trimmed = response.trim_end();
    println!("{trimmed}");
    if trimmed.starts_with("err") {
        std::process::exit(1);
    }
    Ok(())
}

fn default_bundle_path(addr: SocketAddr) -> PathBuf {
    PathBuf::from(format!("ptyroom-{}-{}.ptyrecord", addr.ip(), addr.port()))
}

fn bundle_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("ptyroom")
        .to_string()
}

fn ensure_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn ensure_mp4_path(path: &Path) -> anyhow::Result<()> {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    if ext.as_deref() != Some("mp4") {
        anyhow::bail!(
            "ptyroom embeds browser-controllable MP4 media; got {}",
            path.display()
        );
    }
    Ok(())
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

    use super::{Cli, Command, default_bundle_path, validate_listen_addr, validate_terminal_size};

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
    fn default_bundle_path_uses_ptyroom_name() {
        let path = default_bundle_path(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8022));

        assert_eq!(
            path.file_name().unwrap(),
            "ptyroom-127.0.0.1-8022.ptyrecord"
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
