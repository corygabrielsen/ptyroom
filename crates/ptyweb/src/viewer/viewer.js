// ptyweb viewer: xterm.js bound to a ptyweb WebSocket bridge.
//
// Protocol (matches crates/ptyweb/src/lib.rs):
//   browser -> server : binary frame  = raw keystroke bytes
//                       text frame    = JSON  {"resize": {"cols": N, "rows": N}}
//   server -> browser : binary frame  = raw PTY output bytes
//                       text frame    = JSON  status/resize echo (advisory)

(function () {
  const statusEl = document.getElementById("status");
  const termEl = document.getElementById("term");

  const term = new Terminal({
    convertEol: false,
    cursorBlink: true,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 14,
    scrollback: 5000,
    theme: { background: "#000000", foreground: "#dddddd" },
  });
  term.open(termEl);

  function setStatus(msg) {
    if (!msg) {
      statusEl.classList.remove("show");
      statusEl.textContent = "";
      return;
    }
    statusEl.textContent = msg;
    statusEl.classList.add("show");
  }

  function approximateGeometry() {
    // Without xterm-addon-fit (kept out to stay vendor-light) we fall
    // back to a coarse character-cell estimate from the current font.
    const charWidth = 8;
    const charHeight = 17;
    const cols = Math.max(20, Math.floor(window.innerWidth / charWidth));
    const rows = Math.max(5, Math.floor(window.innerHeight / charHeight));
    return { cols, rows };
  }

  let ws = null;
  let backoffMs = 1000;
  const BACKOFF_MAX = 30000;
  let lastGeom = { cols: 0, rows: 0 };

  function sendResize() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const { cols, rows } = approximateGeometry();
    if (cols === lastGeom.cols && rows === lastGeom.rows) return;
    lastGeom = { cols, rows };
    term.resize(cols, rows);
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
      lastGeom = { cols: 0, rows: 0 };
      sendResize();
    };

    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        // Server-side advisory JSON (status, resize echo). Currently
        // ignored by the viewer — kept reserved for future extensions.
        return;
      }
      term.write(new Uint8Array(ev.data));
    };

    ws.onclose = () => {
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

  window.addEventListener("resize", sendResize);
  connect();
})();
