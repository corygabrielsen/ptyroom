# Security

`ptyroom` is intended for local demos and trusted networks.

The built-in room transport has no authentication, authorization,
encryption, or replay protection. A connected client can type into the
shared PTY. By default, listeners bind to loopback; non-loopback binds
require `--allow-unauthenticated-public-bind`.

For remote use, carry the TCP stream through SSH, WireGuard, a private
overlay network, or another authenticated tunnel. Treat a room like a
shared shell on the host machine.

Security-sensitive issues should be reported privately to the repository
owner before public disclosure.
