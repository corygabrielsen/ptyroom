# Demo + render environment for tint-recorder.
#
# One image with everything: bash + tint for the recording phase, plus
# node/@xterm/headless + python/Pillow + ffmpeg + pinned font for the
# render phase. Running both inside the same pinned container makes the
# resulting GIF byte-stable across machines that have only Docker.

FROM debian:12-slim

# Demo deps (gawk — mawk doesn't support `{18}` regex).
# Render deps: node (snapshot.js), python3 (paint.py), ffmpeg (encode.py),
# fonts-dejavu-core (paint.py reads from the bundled assets dir, but the
# system font is also useful as a fallback).
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash gawk sed grep coreutils ncurses-bin ca-certificates \
        nodejs npm \
        python3 python3-pip python3-venv \
        fonts-dejavu-core ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# tint script (copied from host build context — see Makefile build-image).
COPY tint /usr/local/bin/tint
RUN chmod +x /usr/local/bin/tint

# Renderer code + deps. Installed at the image level (read-only at runtime).
WORKDIR /app
COPY package.json package-lock.json ./
# devDeps include tsx + typescript so renderer/snapshot.ts runs without
# a pre-build compile step. Image stays under control by being pinned.
RUN npm install --no-audit --no-fund
COPY requirements.txt ./
RUN pip3 install --no-cache-dir --break-system-packages -r requirements.txt
COPY renderer ./renderer
COPY assets/fonts ./assets/fonts
COPY render-cast.sh ./
RUN chmod +x render-cast.sh

# Demo user with empty $HOME — no `.tint` on the cd-hook walk-up path.
RUN useradd -m -d /home/demo -s /bin/bash demo
USER demo
WORKDIR /home/demo

ENV TERM=xterm-256color \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC

CMD ["bash"]
