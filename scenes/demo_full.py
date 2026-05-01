"""Full 4-act marketing demo.

Every prerequisite (directories, .tint files, .theme files) is created
on screen during the recording. No magic pre-prepared state — the viewer
sees cause and effect without making invisible assumptions.

Act 1: picker tour     — slow scroll with hero pauses on dracula, solarized-dark, nord
Act 2: CLI direct      — `tint dracula`, `tint solarized-light`, `tint gruvbox-dark`
Act 3: cd hook         — mkdir + echo > .tint + cd, twice
Act 4: custom theme    — heredoc a .theme file, then `tint hot`

One continuous PTY → terminal state persists across all four acts.

Banners (`# ...` typed at the prompt) are bash comments — bash treats
them as no-ops, leaving the explanatory text on screen.

Run from project root:  python -m scenes.demo_full
"""

import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recorder.driver import Recorder


def reset_state() -> None:
    """Clear dirs/files the recording creates, so the scene is re-runnable.

    Cleanup runs in Python BEFORE recording starts — it never appears in
    the cast. (Per self-sufficiency rule, the recording itself shows every
    `mkdir`/`echo`/`heredoc` that produces state the demo references.)
    """
    for p in (Path("/tmp/blueroom"), Path("/tmp/roseroom"),
              Path("/tmp/tint-recorder-home/.config/tint/themes")):
        if p.exists():
            shutil.rmtree(p)

# Hot-pink custom theme — vibrant, distinct from anything in the picker.
# Theme spec: name + 18 #RRGGBB fields (bg + fg + ANSI 0-15).
CUSTOM_THEME_LINE = (
    "hot:#ff006e:#ffffff:"
    "#111111:#222222:#333333:#444444:#555555:#666666:#777777:#888888:"
    "#999999:#aaaaaa:#bbbbbb:#cccccc:#dddddd:#eeeeee:#f0f0f0:#ffffff"
)


def line(r: Recorder, text: str, *, per_char_ms: int = 80,
         dwell_after_ms: int = 600, settle_after_ms: int = 0) -> None:
    """Type `text`, press Enter, dwell. Used for one-shot prompt lines."""
    r.type_text(text, per_char_ms=per_char_ms)
    r.key("enter", dwell_ms=dwell_after_ms)
    if settle_after_ms:
        r.dwell(settle_after_ms)


def blank(r: Recorder, dwell_ms: int = 500) -> None:
    """Visual spacing between sections — Enter on empty prompt."""
    r.key("enter", dwell_ms=dwell_ms)


def act1_picker(r: Recorder) -> None:
    """Picker tour — slow intro, then fast-scroll deep into the 224-theme library.

    Key cadence reflects how a real terminal user would explore: slow at
    first to read controls, then hold-down-arrow speed (~70 ms/key) once
    they're scrolling to find something. Pauses on hero spots so the eye
    can land.
    """
    r.dwell(1800, settle_ms=600)            # opening hold (controls visible)
    r.key("down", dwell_ms=220, repeat=8)   # → idx 8: dracula (slow intro)
    r.dwell(1500)                           # hero pause: dracula

    # Fast scroll through the rest of curated + into the rainbow deep/dark tiers.
    r.key("down", dwell_ms=70, repeat=50)   # → idx 58 (well into rainbow)
    r.dwell(1200)                           # hero pause: deep-color tier

    # Continue fast — into muted/light/pale tiers
    r.key("down", dwell_ms=70, repeat=40)   # → idx 98 (light/pale tiers)
    r.dwell(1200)                           # hero pause: lighter palette

    # Fast scroll deep into neon tier and back-half rainbow
    r.key("down", dwell_ms=70, repeat=40)   # → idx 138 (neon tier)
    r.dwell(1500)                           # hero pause: neon

    # Quick reverse trip showing up-direction also smooths
    r.key("up", dwell_ms=80, repeat=12)
    r.dwell(800)
    # Enter accepts the highlighted theme — bash prompt appears IN that theme.
    # (Esc would restore the original state, undoing all the exploration.)
    r.key("enter", dwell_ms=500)


def act2_cli(r: Recorder) -> None:
    """Type tint commands at the prompt."""
    # Hold the picker's last theme on screen long enough for the viewer to
    # register that Enter accepted the selection — bash prompt is now in
    # that theme's bg + fg + ANSI palette.
    r.dwell(800, settle_ms=400)
    for theme in ("dracula", "solarized-light"):
        line(r, f"tint {theme}", per_char_ms=35, dwell_after_ms=300,
             settle_after_ms=900)


def act3_cd_hook(r: Recorder) -> None:
    """Self-sufficient cd hook demo — hook install + every dir + .tint on screen."""
    # Banner: bash treats `# ...` lines as no-ops, leaving text on screen.
    line(r, "# install the cd hook so .tint files auto-apply", per_char_ms=24,
         dwell_after_ms=300, settle_after_ms=600)
    line(r, 'eval "$(tint hook bash)"', per_char_ms=24,
         dwell_after_ms=300, settle_after_ms=600)

    line(r, "cd /tmp", per_char_ms=24, dwell_after_ms=250, settle_after_ms=300)

    # First .tint dir
    line(r, "mkdir blueroom && echo blue > blueroom/.tint",
         per_char_ms=24, dwell_after_ms=250, settle_after_ms=400)
    line(r, "cd blueroom", per_char_ms=24, dwell_after_ms=300,
         settle_after_ms=900)  # bg shifts to blue

    # Second .tint dir — repeat the pattern with a different theme
    line(r, "cd ..", per_char_ms=24, dwell_after_ms=250, settle_after_ms=300)
    line(r, "mkdir roseroom && echo pale-rose > roseroom/.tint",
         per_char_ms=24, dwell_after_ms=250, settle_after_ms=400)
    line(r, "cd roseroom", per_char_ms=24, dwell_after_ms=300,
         settle_after_ms=900)  # bg shifts to pale-rose


def act4_custom_theme(r: Recorder) -> None:
    """Self-sufficient custom theme — heredoc the .theme file on screen."""
    line(r, "# drop .theme files in ~/.config/tint/themes/",
         per_char_ms=24, dwell_after_ms=300, settle_after_ms=900)

    line(r, "mkdir -p ~/.config/tint/themes", per_char_ms=24,
         dwell_after_ms=250, settle_after_ms=300)

    # Heredoc — bash will print PS2 (`> `) on the body line and after EOF.
    r.type_text("cat > ~/.config/tint/themes/hot.theme <<EOF",
                per_char_ms=24)
    r.key("enter", dwell_ms=200)
    r.dwell(300)                            # let PS2 prompt appear

    r.type_text(CUSTOM_THEME_LINE, per_char_ms=11)  # fast on the long hex run
    r.key("enter", dwell_ms=200)
    r.dwell(200)

    r.type_text("EOF", per_char_ms=24)
    r.key("enter", dwell_ms=300)
    r.dwell(500)

    line(r, "tint hot", per_char_ms=32, dwell_after_ms=300,
         settle_after_ms=1200)              # bg shifts to hot pink


def main():
    reset_state()
    r = Recorder(
        cols=80, rows=24, max_runtime_s=240.0,
        interactive_followup=True,
        # Point tint at the hermetic XDG dir for drop-in themes. The dir
        # doesn't exist until the recording mkdir's it — that's the point.
        palette_dir="/tmp/tint-recorder-home/.config/tint/themes",
    )
    r.start()

    act1_picker(r)
    act2_cli(r)
    blank(r)                # spacing before Act 3
    act3_cd_hook(r)
    blank(r)                # spacing before Act 4
    act4_custom_theme(r)

    r.dwell(3500)  # long outro so the loop doesn't snap back abruptly
    r.stop()

    out = r.write_cast("assets/demo_full.cast")
    print(f"wrote {out} ({out.stat().st_size} bytes, {r.event_count()} events)")


if __name__ == "__main__":
    main()
