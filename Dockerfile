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
USER demo
WORKDIR /home/demo

ENV TERM=xterm-256color \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC

CMD ["bash"]
