# Demo environment for tint-recorder.
#
# Container runs the bash session that scenes drive via PTY. Hermetic by
# construction: no leak from the user's real $HOME, no stray .tint files,
# no host-installed terminal tools shadowing tint's behavior.
#
# The recorder pipes a PTY to `docker run -i`, so the bash inside the
# container sees its stdin as a tty and behaves as a normal interactive
# shell. Scenes type at this shell.

FROM debian:12-slim

# gawk (not mawk) — tint's palette-parsing awk script uses `{18}` interval
# quantifiers, which mawk doesn't support.
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash gawk sed grep coreutils ncurses-bin ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Tint script (copied from host build context — see Makefile `build-image`).
COPY tint /usr/local/bin/tint
RUN chmod +x /usr/local/bin/tint

# Demo user with an empty $HOME. No `.tint` file anywhere on the walk-up
# path (no /home/demo/.tint, no /tmp/.tint), so the cd hook only fires on
# .tint files the recording itself creates.
RUN useradd -m -d /home/demo -s /bin/bash demo
USER demo
WORKDIR /home/demo

ENV TERM=xterm-256color \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8 \
    TZ=UTC

CMD ["bash"]
