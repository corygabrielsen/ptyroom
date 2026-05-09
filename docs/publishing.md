# Publishing

This workspace publishes four crates in dependency order:

```text
ptytrace -> ptyrender -> ptyrecord
        \-> ptyroom
```

Use the helper for real releases. It runs the checks, dry-runs each
package, publishes in dependency order, and waits for each uploaded
version to appear in the crates.io API before moving to a dependent
crate:

```bash
scripts/publish-crates.sh
```

Each internal dependency has both a local `path` and a crates.io `version`.
That lets workspace builds use local sources while uploaded packages resolve
through crates.io.

Release checklist:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace --bins
PTYROOM_SMOKE_SKIP_BUILD=1 scripts/smoke-local.sh
cargo doc --workspace --no-deps
cargo sort --workspace --check
cargo machete
git diff --check
```

Before the first crates.io release, full dry-run verification is possible only for
`ptytrace`; dependent crates cannot dry-run against crates.io until their
internal dependencies are already indexed. The helper handles that case:

```bash
scripts/publish-crates.sh --dry-run --allow-dirty
```

For a real release, publish from a clean commit and do not use
`--allow-dirty`.

First releases are sequential by design. The dry-run can fully verify
`ptytrace` before anything exists on crates.io. After `ptytrace` is
published and indexed, the helper can verify and publish `ptyrender` and
`ptyroom`; after `ptyrender` is indexed, it can verify and publish
`ptyrecord`. The helper waits for each indexed version before moving to a
dependent crate.

If publishing manually, wait for crates.io indexing after every command
before publishing a crate that depends on it:

```bash
cargo publish -p ptytrace
# wait until ptytrace 0.1.0 is visible on crates.io
cargo publish -p ptyrender
# wait until ptyrender 0.1.0 is visible on crates.io
cargo publish -p ptyroom
cargo publish -p ptyrecord
```
