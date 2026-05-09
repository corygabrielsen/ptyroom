use std::time::Duration;

use ptytrace::pty::{PtyTracer, PtyTracerConfig};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn main() -> anyhow::Result<()> {
    let mut recorder = PtyTracer::spawn(
        PtyTracerConfig {
            cols: 80,
            rows: 24,
            ..PtyTracerConfig::default()
        },
        &[
            "env",
            "-i",
            "HOME=/",
            "TERM=xterm-256color",
            "PS1=$ ",
            "bash",
            "--noprofile",
            "--norc",
            "-i",
        ],
    )?;

    recorder.send_raw_wait_for(&[], Duration::ZERO, b"$ ", Duration::from_secs(2), "prompt")?;
    recorder.type_text("echo hello", ms(35))?;
    recorder.send_raw_wait_for(b"\n", ms(300), b"$ ", Duration::from_secs(2), "echo prompt")?;
    recorder.push_presentation_output("\r\n# presentation-only note\r\n", ms(100))?;

    std::fs::create_dir_all("target")?;
    recorder.stop()?.write("target/generic_shell.ptytrace")?;
    Ok(())
}
