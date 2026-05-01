"""PTY-based recorder for tint demos.

Spawns bash + tint inside a fully isolated environment, sends scripted
keystrokes, captures every byte tint writes, and emits an asciinema v2
cast file with deterministic timestamps.

Determinism strategy:
  - All env vars set explicitly (env -i pattern)
  - PTY winsize fixed via TIOCSWINSZ before exec
  - Tint's OSC 11/10 queries stubbed in the spawned bash
  - Cast timestamps derived from intended dwell_ms, NEVER wall-clock
  - Background drainer keeps PTY buffers flowing so tint never blocks
"""

from __future__ import annotations

import fcntl
import json
import os
import pty
import select
import struct
import termios
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

KeyName = Literal[
    "down", "up", "right", "left", "enter", "escape", "tab", "space",
]

KEY_MAP: dict[str, str] = {
    "down":   "\x1b[B",
    "up":     "\x1b[A",
    "right":  "\x1b[C",
    "left":   "\x1b[D",
    "enter":  "\r",
    "escape": "\x1b",
    "tab":    "\t",
    "space":  " ",
}


@dataclass
class Event:
    """One captured chunk of output from tint."""
    output: bytes              # bytes tint wrote after the input
    dwell_ms: int              # how long this frame should hold in playback


class _PtyDrainer:
    """Background reader that keeps the PTY master fd from blocking writers.

    Without continuous draining, tint's slave-side writes block when the kernel's
    PTY buffer fills. The drainer empties the buffer into a thread-safe bytearray
    that consumers atomically swap out.
    """

    def __init__(self, fd: int):
        self._fd = fd
        self._buf = bytearray()
        self._lock = threading.Lock()
        self._done = threading.Event()
        self._thread = threading.Thread(target=self._loop, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._done.set()
        self._thread.join(timeout=0.5)

    def consume(self) -> bytes:
        with self._lock:
            out = bytes(self._buf)
            self._buf.clear()
        return out

    def _loop(self) -> None:
        while not self._done.is_set():
            try:
                r, _, _ = select.select([self._fd], [], [], 0.05)
                if not r:
                    continue
                chunk = os.read(self._fd, 65536)
                if not chunk:
                    break
                with self._lock:
                    self._buf.extend(chunk)
            except OSError:
                break


@dataclass
class Recorder:
    cols: int = 100
    rows: int = 30
    tint_path: str = "/home/cory/code/tint/tint"
    palette_dir: str = ""      # "" = hermetic; tint's drop-in dir disabled
    stub_bg: str = "#1a1b26"   # color tint will see when querying terminal bg
    stub_fg: str = "#c0caf5"
    bash_path: str = "/bin/bash"
    max_runtime_s: float = 120.0  # watchdog: fail fast on hung child

    # When True, drop to an interactive bash prompt after `tint_pick` exits.
    # The follow-on shell has the `tint` CLI on PATH and the cd hook installed,
    # enabling scenes to demo `tint dracula` (Act 2) and cd-into-.tint-dir
    # auto-apply (Act 3) within the same continuous PTY/terminal state.
    interactive_followup: bool = False

    _master_fd: int = field(default=-1, init=False)
    _child_pid: int = field(default=-1, init=False)
    _events: list[Event] = field(default_factory=list, init=False)
    _drainer: _PtyDrainer | None = field(default=None, init=False)
    _start_monotonic: float = field(default=0.0, init=False)

    def start(self) -> None:
        """Fork + exec bash with hermetic env, sourced tint, query stubs in place."""
        # Hermetic env: explicit allow-list, no host leakage. tint's parent dir
        # is on PATH so `tint <theme>` works as a CLI in the follow-on prompt.
        tint_dir = str(Path(self.tint_path).parent)
        clean_env = {
            "TERM": "xterm-256color",
            "LC_ALL": "C.UTF-8",
            "LANG": "C.UTF-8",
            "TZ": "UTC",
            "PATH": f"{tint_dir}:/usr/bin:/bin",
            "HOME": "/tmp/tint-recorder-home",
            "XDG_CONFIG_HOME": "/tmp/tint-recorder-home/.config",
            "TINT_PALETTE_DIR": self.palette_dir,
            "COLUMNS": str(self.cols),
            "LINES": str(self.rows),
            "SHELL": self.bash_path,
            "PS1": "$ ",
        }

        # Create the empty home dir before fork (to avoid HOME=/nonexistent surprises)
        Path(clean_env["HOME"]).mkdir(parents=True, exist_ok=True)

        # Build the bash command:
        #   1. Source tint so functions are defined
        #   2. Stub the OSC color queries (otherwise tint blocks on PTY input)
        #   3. Call tint_pick — the interactive picker entry point
        #   4. (optional) exec into a follow-on interactive bash with hook installed
        stubs = (
            f' _tint_query_terminal_bg() {{ printf "%s" "{self.stub_bg}"; }};'
            f' _tint_query_terminal_fg() {{ printf "%s" "{self.stub_fg}"; }};'
        )
        bash_cmd = f"source {self.tint_path};{stubs} tint_pick"

        if self.interactive_followup:
            # Write rcfile so the follow-on `bash --rcfile FILE -i` re-applies
            # the same setup (sourced tint, stubbed queries, hook eval, PS1).
            rcfile = Path(clean_env["HOME"]) / ".recorderrc"
            # PS1 spells "tint" in four ANSI palette indices (red/yellow/green/
            # cyan = ANSI 1/3/2/6) followed by `$ `. Themes set ANSI 0–15 via
            # OSC 4, so the prompt's letter colors visibly change across themes
            # — making it obvious the demo is changing more than bg + fg.
            ps1 = (
                r"\[\e[31m\]t"   # red
                r"\[\e[33m\]i"   # yellow
                r"\[\e[32m\]n"   # green
                r"\[\e[36m\]t"   # cyan
                r"\[\e[0m\] $ "  # reset, plain prompt tail
            )
            rcfile.write_text(
                f"source {self.tint_path}\n"
                f'_tint_query_terminal_bg() {{ printf "%s" "{self.stub_bg}"; }}\n'
                f'_tint_query_terminal_fg() {{ printf "%s" "{self.stub_fg}"; }}\n'
                f"PS1='{ps1}'\n"
                # /etc/bash.bashrc on Debian/Ubuntu/WSL prints a sudo-MOTD on
                # interactive startup. --rcfile doesn't suppress that. Wipe
                # the screen so the demo starts at a clean prompt.
                f"clear\n"
                # Note: the cd-hook is intentionally NOT installed here —
                # scenes that demo it must `eval "$(tint hook bash)"` on
                # screen so the viewer sees how it's set up.
            )
            bash_cmd += f"; exec {self.bash_path} --rcfile {rcfile} -i"

        self._child_pid, self._master_fd = pty.fork()
        if self._child_pid == 0:
            # Child: set winsize via the slave side (its stdin)
            winsize = struct.pack("HHHH", self.rows, self.cols, 0, 0)
            fcntl.ioctl(0, termios.TIOCSWINSZ, winsize)
            os.execvpe(self.bash_path, [self.bash_path, "-c", bash_cmd], clean_env)
            os._exit(127)

        # Parent: start the background drainer so the PTY never fills
        self._drainer = _PtyDrainer(self._master_fd)
        self._drainer.start()
        self._start_monotonic = time.monotonic()

    def _check_watchdog(self) -> None:
        if time.monotonic() - self._start_monotonic > self.max_runtime_s:
            raise TimeoutError(
                f"Recording exceeded max_runtime_s={self.max_runtime_s} "
                f"(child hung or scene too long?)"
            )

    def _send_and_capture(self, key_bytes: bytes, dwell_ms: int,
                          settle_ms: int = 100) -> None:
        """Write keystroke, wait for tint to finish reacting, record output.

        settle_ms: real wall-clock wait after sending — we drain everything
        produced during this window. The cast timestamp uses dwell_ms (intent),
        NOT settle_ms (measurement). Settle just bounds drain breadth.
        """
        self._check_watchdog()
        os.write(self._master_fd, key_bytes)
        time.sleep(settle_ms / 1000.0)
        captured = self._drainer.consume()
        self._events.append(Event(output=captured, dwell_ms=dwell_ms))

    # ── public scene API ──────────────────────────────────────────────

    def dwell(self, dwell_ms: int = 800, settle_ms: int = 100) -> None:
        """Pause without sending input. Captures any output that arrives."""
        self._send_and_capture(b"", dwell_ms, settle_ms=settle_ms)

    def send_raw(self, data: bytes, dwell_ms: int = 100) -> None:
        self._send_and_capture(data, dwell_ms)

    def key(self, name: KeyName, dwell_ms: int = 200, repeat: int = 1) -> None:
        seq = KEY_MAP[name]
        for _ in range(repeat):
            self._send_and_capture(seq.encode(), dwell_ms)

    def type_text(self, text: str, per_char_ms: int = 60,
                  trailing_dwell_ms: int = 0) -> None:
        for ch in text:
            self._send_and_capture(ch.encode("utf-8"), per_char_ms)
        if trailing_dwell_ms:
            self.dwell(trailing_dwell_ms)

    def event_count(self) -> int:
        return len(self._events)

    def stop(self) -> None:
        """Terminate the child if still alive and stop the drainer."""
        try:
            os.kill(self._child_pid, 9)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self._child_pid, 0)
        except ChildProcessError:
            pass
        if self._drainer:
            self._drainer.stop()
        try:
            os.close(self._master_fd)
        except OSError:
            pass

    # ── output: asciinema v2 cast ─────────────────────────────────────

    def write_cast(self, path: str | Path) -> Path:
        """Emit an asciinema v2 cast file with deterministic timestamps.

        Each event's timestamp = cumulative sum of prior dwell_ms / 1000.
        No wall-clock anywhere.
        """
        path = Path(path)
        header = {
            "version": 2,
            "width": self.cols,
            "height": self.rows,
            "env": {"TERM": "xterm-256color", "SHELL": self.bash_path},
        }
        lines = [json.dumps(header, sort_keys=True, separators=(",", ":"))]

        t_ms = 0
        for ev in self._events:
            if ev.output:
                # tint output is UTF-8 / ASCII; replace handles any stray bytes
                payload = ev.output.decode("utf-8", errors="replace")
                event = [round(t_ms / 1000.0, 6), "o", payload]
                lines.append(json.dumps(event, ensure_ascii=False,
                                         separators=(",", ":")))
            t_ms += ev.dwell_ms

        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return path
