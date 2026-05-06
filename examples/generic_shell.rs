use std::time::Duration;

use tracer::tracer::{Tracer, TracerConfig};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn main() -> anyhow::Result<()> {
    let mut recorder = Tracer::spawn(
        TracerConfig {
            cols: 80,
            rows: 24,
            ..TracerConfig::default()
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

    recorder.stop()?.write("assets/generic_shell.cast")?;
    Ok(())
}
