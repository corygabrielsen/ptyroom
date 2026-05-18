// ptyweb viewer: xterm.js bound to a ptyweb WebSocket bridge.
//
// Protocol (matches crates/ptyweb/src/lib.rs):
//   browser -> server : binary frame  = raw keystroke bytes
//                       text frame    = JSON  {"resize": {"cols": N, "rows": N}}
//   server -> browser : binary frame  = raw PTY output bytes
//                       text frame    = JSON  {"status": "connected", "room": "...", "read_only": bool}

(function () {
  const statusEl = document.getElementById("status");
  const badgeEl = document.getElementById("badge");
  const termEl = document.getElementById("term");

  const term = new Terminal({
    convertEol: false,
    cursorBlink: true,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 14,
    scrollback: 5000,
    theme: { background: "#000000", foreground: "#dddddd" },
  });
  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  term.open(termEl);
  // Initial fit picks up real cell dimensions now that the terminal
  // is in the DOM and the renderer has measured its glyphs.
  safeFit();

  function safeFit() {
    try {
      fitAddon.fit();
    } catch (_) {
      // fit() can throw before xterm has measured cells (e.g. when the
      // terminal element is hidden). Geometry will catch up on the next
      // ResizeObserver tick.
    }
  }

  function setStatus(msg) {
    if (!msg) {
      statusEl.classList.remove("show");
      statusEl.textContent = "";
      return;
    }
    statusEl.textContent = msg;
    statusEl.classList.add("show");
  }

  function renderBadge(info) {
    if (!info || !info.room) {
      badgeEl.classList.remove("show");
      badgeEl.textContent = "";
      return;
    }
    badgeEl.textContent = "";
    const room = document.createElement("span");
    room.className = "room";
    room.textContent = info.room;
    badgeEl.appendChild(room);
    if (info.read_only) {
      const pill = document.createElement("span");
      pill.className = "ro";
      pill.textContent = "read-only";
      badgeEl.appendChild(pill);
    }
    badgeEl.classList.add("show");
  }

  let ws = null;
  let backoffMs = 1000;
  const BACKOFF_MAX = 30000;
  let lastGeom = { cols: 0, rows: 0 };
  let everConnected = false;

  function sendResize() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    safeFit();
    const cols = term.cols;
    const rows = term.rows;
    if (cols === lastGeom.cols && rows === lastGeom.rows) return;
    lastGeom = { cols, rows };
    try {
      ws.send(JSON.stringify({ resize: { cols, rows } }));
    } catch (_) {
      /* socket closed mid-flight; reconnect loop handles it */
    }
  }

  function connect() {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const url = proto + "//" + window.location.host + "/ws";
    setStatus("connecting…");
    ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      backoffMs = 1000;
      setStatus("");
      termEl.classList.remove("disconnected");
      if (everConnected) {
        term.writeln("");
        term.writeln("\x1b[2m[reconnected]\x1b[0m");
      }
      everConnected = true;
      lastGeom = { cols: 0, rows: 0 };
      sendResize();
    };

    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        // Server status frame. Parse and refresh the badge; ignore
        // anything we don't recognise (forward-compat).
        try {
          const msg = JSON.parse(ev.data);
          if (msg && msg.status === "connected") {
            renderBadge({ room: msg.room, read_only: !!msg.read_only });
          }
        } catch (_) {
          /* malformed text frame — ignore */
        }
        return;
      }
      term.write(new Uint8Array(ev.data));
    };

    ws.onclose = () => {
      termEl.classList.add("disconnected");
      if (everConnected) {
        term.writeln("");
        term.writeln("\x1b[2m[disconnected]\x1b[0m");
      }
      setStatus("reconnecting in " + Math.round(backoffMs / 1000) + "s…");
      setTimeout(() => {
        backoffMs = Math.min(BACKOFF_MAX, backoffMs * 2);
        connect();
      }, backoffMs);
    };

    ws.onerror = () => {
      // onclose runs immediately after; let it drive the reconnect.
      try {
        ws.close();
      } catch (_) {
        /* already closing */
      }
    };
  }

  term.onData((data) => {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const enc = new TextEncoder();
    ws.send(enc.encode(data));
  });

  // Observe the terminal element directly. ResizeObserver fires for
  // any layout change that affects size, including initial mount,
  // viewport zoom, and parent reflow — strict superset of `resize`.
  if (typeof ResizeObserver !== "undefined") {
    const ro = new ResizeObserver(() => sendResize());
    ro.observe(termEl);
  } else {
    window.addEventListener("resize", sendResize);
  }
  connect();
})();
