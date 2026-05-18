# Integrating `ptyweb` into a parent process

You requested a feature: "live browser view of a ptyroom session, with
keystrokes flowing back." `ptyweb` is that feature. This document is the
handoff: what was built, how to spawn it, how to wire it into your
companion app.

## What ptyweb is

A standalone binary that bridges **one** ptyroom session to **one**
WebSocket port for **one** browser viewer (or many browsers viewing the
same session — they all share one ptyweb instance, just like browser
tabs sharing a server).

- **Plain HTTP/WS on its listener.** No TLS, no end-user auth.
  You put a reverse proxy in front for both.
- **Single-room-per-process.** Want N rooms? Spawn N ptyweb instances.
  This is intentional (Linus-shaped: ptyweb is plumbing, not a registry).
  Multi-room routing is your orchestrator's job.
- **Standalone viewer.** ptyweb serves its own HTML + xterm.js at `/`.
  You can either embed via iframe or host the viewer yourself by
  fetching the bundled JS.

## Build / install

The workspace is at `github.com:corygabrielsen/ptyroom`. Pick one:

```sh
# from a checkout of the workspace
cargo install --path crates/ptyweb            # installs ~/.cargo/bin/ptyweb

# or from the workspace target (after `cargo build --release`)
./target/release/ptyweb --help
```

No external runtime dependencies. Single static-ish Rust binary
(axum + tokio under the hood). Bundles xterm.js + xterm-addon-fit
in-binary via `include_str!`.

## Companion responsibilities

You (the companion) own:

1. **Spawning a `ptyweb` process per session you want exposed.**
   Track the process lifecycle. Kill when the session is torn down.
2. **Reverse-proxying browser traffic to the ptyweb instance.**
   Map `/terminals/<name>/*` (or whatever URL shape you use) to
   `http://127.0.0.1:<ptyweb-listen-port>/*`.
3. **Injecting `X-PtyWeb-Auth: <shared-secret>` on forwarded requests.**
   ptyweb refuses the WebSocket upgrade if the header doesn't match
   what it was started with via `--auth-secret`.
4. **Terminating TLS** if the browser is remote (`wss://` instead of
   `ws://`). ptyweb never sees TLS.
5. **Enforcing end-user auth** at your proxy layer. ptyweb trusts that
   anything arriving with the correct `X-PtyWeb-Auth` header is
   authorized.

## Subprocess invocation

```sh
ptyweb \
    --room 127.0.0.1:7373            \  # TCP addr of the ptyroom host
    --listen 127.0.0.1:8001          \  # local WebSocket port (loopback!)
    --auth-secret "$PTYWEB_SECRET"   \  # 32-byte random; share with proxy
    --allowed-origin http://localhost:3000 \  # your companion's origin
    [--read-only]                       # optional: drop browser input
```

Notes:

- **Pick `--listen` ports yourself.** ptyweb does not negotiate. Choose
  a per-session port (`8001 + session_index` is fine) and remember it
  for the proxy mapping.
- **`--auth-secret` is required for any non-loopback peer.** If you omit
  it, ptyweb will refuse external connections (loopback dev only).
- **`--allowed-origin` sets CORS.** Match your companion's origin
  exactly. Omit if you only embed via same-origin iframe through the
  reverse proxy.
- Process exits non-zero if `--room` is unreachable on startup or if
  `--listen` fails to bind.
- Logs go to stderr; use `RUST_LOG=ptyweb=info` or similar.

## Reverse-proxy snippets

### Caddy

```caddyfile
handle_path /terminals/claude/* {
    reverse_proxy 127.0.0.1:8001 {
        header_up X-PtyWeb-Auth {env.PTYWEB_SECRET}
    }
}
```

### nginx

```nginx
location /terminals/claude/ {
    proxy_pass http://127.0.0.1:8001/;
    proxy_set_header X-PtyWeb-Auth $http_x_ptyweb_secret_env;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_http_version 1.1;
    proxy_read_timeout 1h;
}
```

### Node / Express (http-proxy-middleware)

```js
import { createProxyMiddleware } from 'http-proxy-middleware';

app.use('/terminals/claude', createProxyMiddleware({
    target: 'http://127.0.0.1:8001',
    ws: true,
    changeOrigin: false,
    pathRewrite: { '^/terminals/claude': '' },
    headers: { 'X-PtyWeb-Auth': process.env.PTYWEB_SECRET },
}));
```

## Frontend embed

### Easiest: iframe through your proxy

```html
<iframe src="/terminals/claude/"
        style="width:100%; height:600px; border:0;">
</iframe>
```

The viewer is fully self-contained: xterm.js, CSS, JS, status badge,
reconnect overlay all served from ptyweb. Iframe + reverse proxy = done.

### Custom: host the viewer yourself

Fetch the assets from ptyweb (or vendor them) and open the WebSocket
directly from your own page:

```js
const xterm = new Terminal({ /* ... */ });
const fit = new FitAddon.FitAddon();
xterm.loadAddon(fit);
xterm.open(document.getElementById('term'));
fit.fit();

const ws = new WebSocket('wss://your-host/terminals/claude/ws');
ws.binaryType = 'arraybuffer';
ws.onmessage = (e) => {
    if (typeof e.data === 'string') {
        const msg = JSON.parse(e.data);  // status frame, see below
    } else {
        xterm.write(new Uint8Array(e.data));
    }
};
xterm.onData((data) => ws.send(new TextEncoder().encode(data)));
new ResizeObserver(() => {
    fit.fit();
    ws.send(JSON.stringify({ resize: { cols: xterm.cols, rows: xterm.rows }}));
}).observe(document.getElementById('term'));
```

Most use cases should just iframe.

## WebSocket protocol (reference)

You usually don't talk to it directly — the viewer does. Provided for
debugging or custom-viewer use.

| Direction | Frame type | Content |
|---|---|---|
| browser → ptyweb | binary | raw keystroke bytes (escape sequences included) |
| browser → ptyweb | text | JSON: `{"resize": {"cols": <u16>, "rows": <u16>}}` |
| ptyweb → browser | binary | raw PTY output bytes |
| ptyweb → browser | text | JSON status frame (see below) |

### Status frame

Sent **once** per WebSocket handshake, immediately after upgrade:

```json
{
    "status": "connected",
    "room": "127.0.0.1:7373",
    "read_only": false
}
```

The viewer renders a fixed-position badge from this. `read_only: true`
adds a red "read-only" pill.

## Reconnect behavior

The viewer reconnects automatically with exponential backoff (cap 30 s).
During disconnect:

- The terminal instance is preserved (scrollback intact)
- A dim `[disconnected]` line is appended
- The terminal opacity drops to 0.6
- On reconnect: `[reconnected]` line appended, opacity restored, fresh
  status frame re-renders the badge

You don't need to do anything for this — it's all viewer-side.

## What ptyweb does NOT do (current scope)

- **No TLS termination.** Proxy handles.
- **No end-user authentication.** Proxy handles.
- **No multi-room routing.** Spawn N processes.
- **No process supervision / daemonization.** Your job (systemd unit,
  process manager, supervisor, whatever your companion uses).
- **No persistent sessions.** ptyweb is stateless across reconnects
  (every WS connection is a fresh viewer joining the same room).
  Persistence is a ptyroom concern.
- **No file uploads / clipboard sync / sound** — pure terminal bytes.

## Operational guidance

- **Health check.** `GET /healthz` returns `ok` (200). Use for liveness
  probes in your process supervisor.
- **Lifecycle.** When the underlying ptyroom host disconnects (room
  ends), ptyweb logs and exits. Plan to restart only when the room is
  back up — don't auto-restart blindly.
- **Resource use.** Per-process: ~5–10 MB RSS at rest; CPU is
  proportional to PTY output rate. One process per session scales fine
  to dozens of concurrent sessions on a normal machine.
- **Ports.** Bind only to loopback (`127.0.0.1`) unless you're sure you
  want a public listener — and even then, prefer to make the reverse
  proxy the only public surface.

## Reference

- Workspace: `github.com:corygabrielsen/ptyroom`
- Crate: `crates/ptyweb/`
- Phase 1 commit: `12c2e65` (initial build)
- Phase 2 commit: `6bb347e` (fit addon + status frame + reconnect UX)
- Tests: `cargo test -p ptyweb` (9 tests including end-to-end mock-host)
- Original design doc: see project memory `ptyweb_feature_proposal.md`
- Vendored: xterm.js 5.3.0, xterm-addon-fit 0.8.0 (both MIT)
