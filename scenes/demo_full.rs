//! Full 4-act marketing demo.
//!
//! Every prerequisite (directories, .tint files, .theme files) is created
//! on screen during the recording. No magic pre-prepared state — the
//! viewer sees cause and effect in full. Hermeticity comes from the demo
//! container the recorder spawns.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Key, Recorder, RecorderConfig};
use tint_recorder::scenes::{blank, line, lookup_picker_idx, ms};

const ACT1_TARGET: &str = "dark-orange";

const CUSTOM_THEME_LINE: &str = concat!(
    "hot:#ff006e:#ffffff:",
    "#111111:#222222:#333333:#444444:#555555:#666666:#777777:#888888:",
    "#999999:#aaaaaa:#bbbbbb:#cccccc:#dddddd:#eeeeee:#f0f0f0:#ffffff",
);

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/demo_full.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let target_idx = lookup_picker_idx(&args.tint_path, ACT1_TARGET)?;

    let mut r = Recorder::start(RecorderConfig::default())?;
    act1_picker(&mut r, target_idx)?;
    act2_cli(&mut r)?;
    blank(&mut r, ms(500))?;
    act3_cd_hook(&mut r)?;
    blank(&mut r, ms(500))?;
    act4_custom_theme(&mut r)?;
    r.dwell(ms(3500), ms(100))?; // outro

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}

fn act1_picker(r: &mut Recorder, target_idx: usize) -> anyhow::Result<()> {
    r.dwell(ms(800), ms(600))?;
    line(r, "# tint — terminal theme switcher", ms(35), ms(400), ms(1000))?;

    r.type_text("tint", ms(80))?;
    r.key(Key::Enter, ms(400))?;
    r.dwell(ms(900), ms(100))?;

    r.keys(Key::Down, ms(50), target_idx)?;
    r.dwell(ms(1000), ms(100))?;
    r.key(Key::Enter, ms(500))?;
    Ok(())
}

fn act2_cli(r: &mut Recorder) -> anyhow::Result<()> {
    r.dwell(ms(800), ms(400))?;
    for theme in ["dracula", "solarized-light"] {
        line(r, &format!("tint {theme}"), ms(35), ms(300), ms(900))?;
    }
    Ok(())
}

fn act3_cd_hook(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# install the cd hook so .tint files auto-apply",
         ms(24), ms(300), ms(600))?;
    line(r, "eval \"$(tint hook bash)\"", ms(24), ms(300), ms(600))?;
    line(r, "cd /tmp", ms(24), ms(250), ms(300))?;

    line(r, "mkdir blueroom && echo blue > blueroom/.tint",
         ms(24), ms(250), ms(400))?;
    line(r, "cd blueroom", ms(24), ms(300), ms(900))?;

    line(r, "cd ..", ms(24), ms(250), ms(300))?;
    line(r, "mkdir roseroom && echo pale-rose > roseroom/.tint",
         ms(24), ms(250), ms(400))?;
    line(r, "cd roseroom", ms(24), ms(300), ms(900))?;
    Ok(())
}

fn act4_custom_theme(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# drop .theme files in ~/.config/tint/themes/",
         ms(24), ms(300), ms(900))?;
    line(r, "mkdir -p ~/.config/tint/themes",
         ms(24), ms(250), ms(300))?;

    r.type_text("cat > ~/.config/tint/themes/hot.theme <<EOF", ms(24))?;
    r.key(Key::Enter, ms(200))?;
    r.dwell(ms(300), ms(100))?;

    r.type_text(CUSTOM_THEME_LINE, ms(11))?;
    r.key(Key::Enter, ms(200))?;
    r.dwell(ms(200), ms(100))?;

    r.type_text("EOF", ms(24))?;
    r.key(Key::Enter, ms(300))?;
    r.dwell(ms(500), ms(100))?;

    line(r, "tint hot", ms(32), ms(300), ms(1200))?;
    Ok(())
}
