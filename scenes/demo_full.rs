//! Full 4-feature marketing demo: picker → cli → cd-hook → custom-theme.
//!
//! Composes the per-feature scene helpers from [`tint_recorder::scenes`].
//! Every prerequisite (directories, .tint files, .theme files) is created
//! on screen during the recording. Hermeticity comes from the demo
//! container the recorder spawns.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{
    blank, lookup_picker_idx, ms,
    run_cd_hook, run_clear, run_cli, run_custom_theme, run_picker,
    run_preamble, run_reset,
};

/// Theme the picker lands on. Picked deliberately for the cool/blue
/// register — reads better as the demo's first reveal than a warm/orange
/// theme, which can look default-terminal-ish at a glance.
const PICKER_TARGET: &str = "dark-azure";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/demo_full.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let target_idx = lookup_picker_idx(&args.tint_path, PICKER_TARGET)?;

    // Composition pacing:
    // - 800/600ms initial dwell: bash needs ~600ms to set up echo before
    //   the first keystroke or input bytes leak into the top-left.
    //   Required on every recording's first call (per scenes.rs convention).
    // - run_preamble enumerates the four features as a numbered list, so
    //   the viewer knows what they're investing attention in. Per-act
    //   headers later are bare descriptions (no numbers) — the preamble
    //   already carried the count.
    // Act order: cli → picker → cd-hook → custom-theme. Cli first
    // because it's the fastest demonstration of "this is what tint
    // does" — `tint dracula` flips the bg in 1-2s, no UI to navigate.
    // Picker is the visually impressive moment, but it lands harder
    // *after* the viewer already understands the basic verb.
    //
    // - One blank Enter (500ms dwell) between every act for consistent
    //   visual breathing room — anything more reads heavy, anything less
    //   makes acts run together. *Exception:* the act AFTER the picker
    //   gets two blanks because the picker exits via alt-screen
    //   (\e[?1049l), which leaves no trace in the main buffer — the
    //   visual state goes straight from "tint" prompt to next prompt,
    //   swallowing the between-act gap that other transitions get for
    //   free from the prior act's trailing output. A second blank
    //   restores the between-act spacing parity.
    // - Act 5: `tint reset` returns to default colors.
    // - 3000ms beat: viewer reads accumulated output one last time.
    // - Act 6: `clear` wipes the screen. The GIF then loops from a
    //   blank prompt back to the demo's blank-prompt start state, so
    //   the wrap-around reads as if the user typed `clear` to start
    //   the demo over. Reusable end-cap — keep `run_clear` available
    //   for future composed scenes.
    // - 800ms tail dwell: brief pause on the cleared state before
    //   the GIF wraps, so the loop transition has its own beat.
    // 36 rows: counted what the demo prints (preamble + 5 acts = ~30
    // command rows + blanks + heredoc wrap), default 30 was clipping
    // the trailing reset. TODO: programmatic fit (measure max used row
    // across snapshots, crop output to that).
    let mut r = Recorder::start(RecorderConfig { rows: 36, ..RecorderConfig::default() })?;
    // Initial: zero visible dwell, but keep 600ms wall-clock settle so
    // bash sets up echo before any keystrokes (otherwise input bytes
    // leak into the top-left of the terminal).
    r.dwell(ms(0), ms(600))?;
    run_preamble(&mut r)?;
    // Short blank between preamble and act 1: the preamble is framing,
    // not a peer act, so it gets a tighter join than the standard 500ms
    // inter-act spacing. Keeps the demo from "waiting" before content.
    blank(&mut r, ms(250))?;
    run_cli(&mut r)?;
    blank(&mut r, ms(500))?;
    run_picker(&mut r, target_idx)?;
    blank(&mut r, ms(250))?;
    blank(&mut r, ms(500))?;
    run_cd_hook(&mut r)?;
    blank(&mut r, ms(500))?;
    run_custom_theme(&mut r)?;
    blank(&mut r, ms(500))?;
    run_reset(&mut r)?;
    r.dwell(ms(3000), ms(100))?;
    run_clear(&mut r)?;
    // Zero tail dwell: loop straight from cleared state into the next cycle.
    r.dwell(ms(0), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
