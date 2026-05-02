"""PTY-based recorder for tint demos.

Spawns bash inside a Docker container (hermetic environment), sends
scripted keystrokes, captures every byte tint writes, and emits an
asciinema v2 cast file with deterministic timestamps.

Determinism strategy:
  - Demo runs inside a pinned Docker image (no host $HOME / $PATH leakage)
  - PTY winsize fixed via TIOCSWINSZ before exec
  - Recorder acts as terminal emulator: answers tint's OSC 11/10 queries
    with canned RGB replies so the real tint binary runs unmodified
  - Cast timestamps derived from intended dwell_ms, NEVER wall-clock
  - Background drainer keeps PTY buffers flowing so tint never blocks
"""

from __future__ import annotations

import fcntl
import json
import os
import pty
import re
import select
import struct
import tempfile
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


_OSC_QUERY = re.compile(rb'\x1b\](10|11);\?(?:\x1b\\|\x07)')


class _PtyDrainer:
    """Drain the PTY master fd and act as terminal emulator for tint.

    Continuous draining keeps tint's slave-side writes from blocking. The
    same loop also intercepts OSC 11/10 *query* sequences (`\\e]11;?\\e\\\\`)
    and writes back canned RGB replies so the real tint binary, running
    inside the container, gets a valid terminal response without any
    in-shell stubs.
    """

    def __init__(self, fd: int, stub_bg: str, stub_fg: str):
        self._fd = fd
        self._buf = bytearray()
        self._lock = threading.Lock()
        self._done = threading.Event()
        self._thread = threading.Thread(target=self._loop, daemon=True)
        self._osc_replies = {
            b"11": _hex_to_osc_reply("11", stub_bg),
            b"10": _hex_to_osc_reply("10", stub_fg),
        }

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
                self._answer_osc_queries(chunk)
                with self._lock:
                    self._buf.extend(chunk)
            except OSError:
                break

    def _answer_osc_queries(self, chunk: bytes) -> None:
        """Synthesize and inject OSC 11/10 query responses back into the PTY."""
        for match in _OSC_QUERY.finditer(chunk):
            reply = self._osc_replies.get(match.group(1))
            if reply is not None:
                try:
                    os.write(self._fd, reply)
                except OSError:
                    pass


def _hex_to_osc_reply(code: str, hex_color: str) -> bytes:
    """Build an OSC <code> reply: \\x1b]CODE;rgb:RR/GG/BB\\x1b\\\\."""
    h = hex_color.lstrip("#")
    rr, gg, bb = h[0:2], h[2:4], h[4:6]
    return f"\x1b]{code};rgb:{rr}/{gg}/{bb}\x1b\\".encode()


@dataclass
class Recorder:
    cols: int = 100
    rows: int = 30
    image: str = "tint-recorder:demo"  # Docker image holding bash + tint
    stub_bg: str = "#1a1b26"   # color reported when tint queries OSC 11
    stub_fg: str = "#c0caf5"   # color reported when tint queries OSC 10
    max_runtime_s: float = 240.0  # watchdog: fail fast on hung child

    _master_fd: int = field(default=-1, init=False)
    _child_pid: int = field(default=-1, init=False)
    _events: list[Event] = field(default_factory=list, init=False)
    _drainer: _PtyDrainer | None = field(default=None, init=False)
    _start_monotonic: float = field(default=0.0, init=False)
    _rcfile_host: Path | None = field(default=None, init=False)

    def start(self) -> None:
        """Spawn `docker run` whose container runs interactive bash.

        The container provides hermeticity (empty $HOME, no leftover state,
        pinned image). Scenes drive bash via PTY; the recorder also answers
        tint's OSC 11/10 queries so real `tint` runs unmodified.
        """
        # PS1 spells "tint" in four ANSI palette indices (red/yellow/green/cyan)
        # so the prompt's letter colors visibly change across themes via OSC 4.
        ps1 = (
            r"\[\e[31m\]t\[\e[33m\]i\[\e[32m\]n\[\e[36m\]t\[\e[0m\] $ "
        )
        rc_fd, rc_path = tempfile.mkstemp(prefix="tint-recorder-rc-", suffix=".rc")
        with os.fdopen(rc_fd, "w") as f:
            f.write(f'cd "$HOME"\nPS1=\'{ps1}\'\nclear\n')
        self._rcfile_host = Path(rc_path)

        docker_args = [
            "docker", "run", "--rm", "-i", "-t",
            # Pass size via env in case in-container winsize lags at startup.
            "-e", f"LINES={self.rows}",
            "-e", f"COLUMNS={self.cols}",
            "-v", f"{self._rcfile_host}:/tmp/recorderrc:ro",
            self.image,
            "bash", "--rcfile", "/tmp/recorderrc", "-i",
        ]

        self._child_pid, self._master_fd = pty.fork()
        if self._child_pid == 0:
            winsize = struct.pack("HHHH", self.rows, self.cols, 0, 0)
            fcntl.ioctl(0, termios.TIOCSWINSZ, winsize)
            os.execvp("docker", docker_args)
            os._exit(127)

        self._drainer = _PtyDrainer(self._master_fd, self.stub_bg, self.stub_fg)
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
        if self._rcfile_host is not None:
            try:
                self._rcfile_host.unlink(missing_ok=True)
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
            "env": {"TERM": "xterm-256color", "SHELL": "/bin/bash"},
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
