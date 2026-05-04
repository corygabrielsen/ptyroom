# Recording-only environment for tint-recorder.
#
# Single-stage debian:12-slim with bash, ncurses, and the tint script.
# Post-recording stages (snapshot replay, paint, encode, verify) all
# run on the host instead of inside this image, so this Dockerfile no
# longer bundles the Rust toolchain, node/npm, ffmpeg, or any of the
# tint-* binaries — the previous multi-stage build went away with the
# host-side render flow. Result: this image rebuilds in well under a
# second when cached, and changes to encode.rs / paint.rs / etc. don't
# invalidate it at all.

FROM debian:12-slim

# Runtime deps for `tint` (the script that runs inside this container
# during recording). bash + tint executable, plus the standard text
# tools tint uses (awk for palette parsing, etc.). ncurses-bin
# provides `tput` and the terminfo db that bash + tint read.
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash gawk sed grep coreutils ncurses-bin ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# tint script (copied from the host via the Makefile's tar context).
COPY tint /usr/local/bin/tint
RUN chmod +x /usr/local/bin/tint

# Demo user with empty $HOME.
RUN useradd -m -d /home/demo -s /bin/bash demo
RUN cat > /home/demo/.tint-recorder.rc <<'EOF' \
    && chown demo:demo /home/demo/.tint-recorder.rc
cd "$HOME"
PS1='\[\e[31m\]t\[\e[33m\]i\[\e[32m\]n\[\e[36m\]t\[\e[0m\] $ '
tint() {
    if [ "$#" -eq 0 ] && [ -n "${TINT_RECORDER_PICKER_CURRENT:-}" ]; then
        . /usr/local/bin/tint || return $?
        local _tint_result _tint_status
        _tint_result=$(tint_pick "$TINT_RECORDER_PICKER_CURRENT")
        _tint_status=$?
        if [ "$_tint_status" -eq 0 ] && [ -n "$_tint_result" ]; then
            printf '%s\n' "$_tint_result"
        fi
        return "$_tint_status"
    fi
    command tint "$@"
}
printf '\033[H\033[2J\033[3J'
EOF
RUN cat > /usr/local/bin/tint-recorder-shell <<'EOF' \
    && chmod +x /usr/local/bin/tint-recorder-shell
#!/bin/sh
set -eu
rm -rf "$HOME"
mkdir -p "$HOME"
# The cd-hook scene intentionally demonstrates `cd /tmp`, so warm
# container recordings must reset those visible demo paths too.
rm -rf /tmp/foo /tmp/bar
cd "$HOME"
exec bash --rcfile /home/demo/.tint-recorder.rc -i
EOF
USER demo
WORKDIR /home/demo

ENV TERM=xterm-256color \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC

CMD ["bash"]
