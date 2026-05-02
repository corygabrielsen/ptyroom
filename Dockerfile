# Demo + render environment for tint-recorder.
#
# Multi-stage:
#   1. builder — Rust toolchain compiles the recorder + paint + encode +
#      verify + inspect + scene binaries. Cargo cache is layered.
#   2. runtime — minimal debian:12-slim with bash, gawk, ffmpeg, the tint
#      script, and the compiled Rust binaries. Total image is small (no
#      Rust toolchain, no node, no python).

# ───── builder ─────
FROM rust:1-bookworm AS builder

WORKDIR /build

# Pre-fetch the dependency graph in its own layer so editing src/ doesn't
# re-download or recompile crates.io packages on every build. We need a
# placeholder lib + bin so cargo will resolve the manifest's [[bin]] entries.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src src/bin scenes && \
    echo "fn main() {}" > src/bin/encode.rs && \
    cp src/bin/encode.rs src/bin/paint.rs && \
    cp src/bin/encode.rs src/bin/inspect.rs && \
    cp src/bin/encode.rs src/bin/verify.rs && \
    cp src/bin/encode.rs scenes/smoke.rs && \
    cp src/bin/encode.rs scenes/demo_full.rs && \
    echo "" > src/lib.rs && \
    cargo fetch --locked

# Real sources — cargo build now compiles only our code, not deps.
COPY src ./src
COPY scenes ./scenes
COPY assets ./assets
RUN touch src/lib.rs src/bin/*.rs scenes/*.rs && \
    cargo build --release --bins --locked

# ───── runtime ─────
FROM debian:12-slim

# Runtime deps:
#   - bash gawk sed grep coreutils ncurses-bin: tint script's runtime
#   - ffmpeg: encode binary shells out for GIF assembly
#   - nodejs + npm: snapshot replay runs through @xterm/headless via tsx
#     (the only mature terminal emulator with proper OSC 11 support is JS)
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash gawk sed grep coreutils ncurses-bin ca-certificates \
        ffmpeg nodejs npm \
    && rm -rf /var/lib/apt/lists/*

# tint script (host build context — see Makefile build-image).
COPY tint /usr/local/bin/tint
RUN chmod +x /usr/local/bin/tint

# Snapshot replay (JS-locked dep).
WORKDIR /app
COPY package.json package-lock.json tsconfig.json ./
RUN npm install --no-audit --no-fund
COPY renderer ./renderer
COPY render-cast.sh /usr/local/bin/render-cast.sh
RUN chmod +x /usr/local/bin/render-cast.sh

# Recorder binaries (Rust, compiled in builder stage).
COPY --from=builder /build/target/release/paint   /usr/local/bin/tint-paint
COPY --from=builder /build/target/release/encode  /usr/local/bin/tint-encode
COPY --from=builder /build/target/release/inspect /usr/local/bin/tint-inspect
COPY --from=builder /build/target/release/verify  /usr/local/bin/tint-verify

# Demo user with empty $HOME.
RUN useradd -m -d /home/demo -s /bin/bash demo
USER demo
WORKDIR /home/demo

ENV TERM=xterm-256color \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC

CMD ["bash"]
