#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ "${PTYROOM_SMOKE_SKIP_BUILD:-0}" != "1" ]]; then
    cargo build --workspace --bins
fi

tmp="$(mktemp -d)"
cleanup() {
    local status=$?
    if [[ -n "${host_pid:-}" ]] && kill -0 "$host_pid" 2>/dev/null; then
        kill "$host_pid" 2>/dev/null || true
        wait "$host_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
    exit "$status"
}
trap cleanup EXIT

trace="$tmp/smoke.ptytrace"
gif="$tmp/smoke.gif"
host_log="$tmp/host.stderr"
host_stdout="$tmp/host.stdout"

target/debug/ptyroom host \
    --listen 127.0.0.1:0 \
    --out "$trace" \
    --cols 80 \
    --rows 24 \
    --no-local-input \
    --no-local-output \
    bash --noprofile --norc -i \
    >"$host_stdout" \
    2>"$host_log" &
host_pid=$!

addr=""
for _ in {1..100}; do
    if ! kill -0 "$host_pid" 2>/dev/null; then
        cat "$host_log" >&2 || true
        echo "ptyroom host exited before listening" >&2
        exit 1
    fi
    addr="$(
        sed -n 's/^\[ptyroom listening on \(.*\)\]$/\1/p' "$host_log" | tail -n 1
    )"
    if [[ -n "$addr" ]]; then
        break
    fi
    sleep 0.05
done

if [[ -z "$addr" ]]; then
    cat "$host_log" >&2 || true
    echo "timed out waiting for ptyroom host listener" >&2
    exit 1
fi

join_output="$tmp/join.out"
printf 'echo PTYROOM_SMOKE\nexit\n' | target/debug/ptyroom join "$addr" >"$join_output"
wait "$host_pid"
host_pid=""

if ! grep -q 'PTYROOM_SMOKE' "$join_output"; then
    cat "$join_output" >&2
    echo "joined client did not receive expected PTY output" >&2
    exit 1
fi

if [[ ! -s "$trace" ]]; then
    echo "host did not write trace: $trace" >&2
    exit 1
fi

if command -v ffmpeg >/dev/null 2>&1; then
    render_stdout="$tmp/render.stdout"
    render_stderr="$tmp/render.stderr"
    if ! target/debug/ptyrender "$trace" "$gif" >"$render_stdout" 2>"$render_stderr"; then
        cat "$render_stdout" >&2 || true
        cat "$render_stderr" >&2 || true
        echo "ptyrender failed" >&2
        exit 1
    fi
    if [[ ! -s "$gif" ]]; then
        echo "ptyrender did not write GIF: $gif" >&2
        exit 1
    fi
    echo "ok: joined room, wrote trace, rendered GIF"
else
    echo "ok: joined room and wrote trace; skipped GIF render because ffmpeg is not on PATH"
fi
