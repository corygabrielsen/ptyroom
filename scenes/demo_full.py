"""Full 4-act marketing demo.

Every prerequisite (directories, .tint files, .theme files) is created on
screen during the recording. No magic pre-prepared state — viewer sees
cause and effect without making invisible assumptions. Hermeticity is
provided by the Docker container the recorder spawns.

Act 1: typed banner + `tint` launches picker → brisk scroll to a hero theme
Act 2: CLI direct      — `tint dracula`, `tint solarized-light`
Act 3: cd hook         — install hook + mkdir + echo > .tint + cd, twice
Act 4: custom theme    — heredoc a .theme file, then `tint hot`

One continuous PTY → terminal state persists across all four acts.

Banners (`# ...` typed at the prompt) are bash comments — bash treats
them as no-ops, leaving the explanatory text on screen.

Run from project root:  make demo
"""

import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recorder.driver import Recorder

# Path to the tint script on the host. Same script gets baked into the demo
# image (see Makefile build-image), so listing themes here matches what the
# recording will see.
TINT_PATH = os.environ.get("TINT_PATH", "/home/cory/code/tint/tint")

# Act 1 landing theme — looked up by name at scene start so adding new themes
# upstream of this one doesn't shift its picker index.
ACT1_TARGET = "dark-orange"

# Hot-pink custom theme — vibrant, distinct from anything in the picker.
CUSTOM_THEME_LINE = (
    "hot:#ff006e:#ffffff:"
    "#111111:#222222:#333333:#444444:#555555:#666666:#777777:#888888:"
    "#999999:#aaaaaa:#bbbbbb:#cccccc:#dddddd:#eeeeee:#f0f0f0:#ffffff"
)


def lookup_picker_idx(theme_name: str) -> int:
    """Return 1-based picker idx for a built-in theme.

    Picker row 0 is the "(unchanged)" no-op; row N corresponds to line N of
    `tint -l`. Runs the tint script directly (no docker) for speed —
    output is identical because the same script is in the image.
    """
    result = subprocess.run(
        [TINT_PATH, "-l"],
        capture_output=True, text=True, check=True,
        env={"TINT_PALETTE_DIR": "", "PATH": "/usr/bin:/bin"},
    )
    themes = result.stdout.splitlines()
    if theme_name not in themes:
        raise ValueError(f"theme not found in `tint -l`: {theme_name!r}")
    return themes.index(theme_name) + 1


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


def act1_picker(r: Recorder, target_idx: int) -> None:
    """Banner + typed `tint` launch + brisk scroll to the Act 1 target theme."""
    r.dwell(800, settle_ms=600)            # let initial prompt settle
    line(r, "# tint — terminal theme switcher",
         per_char_ms=35, dwell_after_ms=400, settle_after_ms=1000)

    r.type_text("tint", per_char_ms=80)    # launch picker
    r.key("enter", dwell_ms=400)
    r.dwell(900)                           # picker opens, controls visible

    # Brisk scroll past curated into the dark-rainbow tier — viewer sees
    # color tier transitions without belaboring the inventory.
    r.key("down", dwell_ms=50, repeat=target_idx)
    r.dwell(1000)                          # hero pause
    r.key("enter", dwell_ms=500)           # accept


def act2_cli(r: Recorder) -> None:
    """Type tint commands at the prompt."""
    r.dwell(800, settle_ms=400)
    for theme in ("dracula", "solarized-light"):
        line(r, f"tint {theme}", per_char_ms=35, dwell_after_ms=300,
             settle_after_ms=900)


def act3_cd_hook(r: Recorder) -> None:
    """Self-sufficient cd hook demo — hook install + every dir + .tint on screen."""
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
    target_idx = lookup_picker_idx(ACT1_TARGET)
    r = Recorder(cols=80, rows=30, max_runtime_s=240.0)
    r.start()

    act1_picker(r, target_idx)
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
