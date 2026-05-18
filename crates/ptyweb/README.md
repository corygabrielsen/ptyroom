# ptyweb

Browser ↔ ptyroom WebSocket bridge.

`ptyweb` is one process that bridges one ptyroom host to one WebSocket
listener. Browsers attach to `/ws` and see a live xterm.js view of the
shared terminal; keystrokes flow back over the same socket.

Multi-room concerns (registry, per-user auth, TLS) belong to the
orchestrator that spawns ptyweb — typically a reverse proxy.

## Usage

```
# terminal A — start a ptyroom host
ptyroom host --argv claude

# terminal B — bridge it to a WebSocket
ptyweb \
    --room 127.0.0.1:7373 \
    --listen 127.0.0.1:8001 \
    --auth-secret "$PTYWEB_SECRET" \
    --allowed-origin http://localhost:3000
```

A reverse proxy in front of `ptyweb` should:

- terminate TLS
- inject `X-PtyWeb-Auth: $PTYWEB_SECRET` on forwarded requests
- forward `Upgrade: websocket` for the `/ws` endpoint

`ptyweb` itself stays plain HTTP/WS.

## Flags

| Flag                | Description                                                  |
| ------------------- | ------------------------------------------------------------ |
| `--room ADDR`       | TCP address of the ptyroom host.                             |
| `--listen ADDR`     | WebSocket listener (`127.0.0.1:8001` is the typical local).  |
| `--auth-secret S`   | Required shared secret. Omit only for loopback-only dev use. |
| `--allowed-origin O`| Optional `Access-Control-Allow-Origin` value.                |
| `--read-only`       | Drop browser-originated keystrokes and resize events.        |

## Endpoints

- `GET /` — viewer HTML
- `GET /viewer.js`, `GET /xterm.js`, `GET /xterm.css` — vendored assets
- `GET /healthz` — liveness probe (returns `ok`)
- `GET /ws` — WebSocket upgrade; gated on `X-PtyWeb-Auth`

## Vendored assets

`xterm.js` 5.3.0 (MIT). Update by replacing `src/viewer/xterm.js` and
`src/viewer/xterm.css`.
