# `.ptyrecord` Compatibility Policy

The `.ptyrecord` bundle format is a versioned JSON artifact. The current
schema version is defined by the `PTYRECORD_VERSION` constant in
`src/lib.rs`. The current value is `1`.

## Policy

**New readers MUST read all `.ptyrecord` versions >= 1.** A reader built
against a higher schema version must continue to load every older bundle
it has ever known how to load. Removing read support for an older
version is a breaking change to the format.

**Old readers MUST refuse newer versions with a clear error message** that
names the writer version and suggests upgrading the reader. Silent
best-effort parsing of a future schema is forbidden — the reader has no
way to know which fields it is misinterpreting.

## When to bump the version

Bump `PTYRECORD_VERSION` when, and only when, the change is structurally
incompatible with existing readers. Concretely:

- A field is **removed**.
- A field's **type changes** (including container type, enum widening
  that an old reader would reject, or numeric range changes that affect
  parsing).
- A **required field is added** — i.e. one without `#[serde(default)]`,
  which existing bundles cannot supply.
- The **semantic meaning** of an existing field changes such that an old
  reader would draw the wrong conclusion.

Do **not** bump for additive, optional changes:

- A new field marked `#[serde(default, skip_serializing_if = "...")]`.
- A new optional enum variant that does not appear in older bundles.
- A documentation or comment change.

Note: the `PtyRecord` struct currently uses `#[serde(deny_unknown_fields)]`.
That means even an additive optional field requires either (a) lifting
`deny_unknown_fields` first, or (b) treating the addition as a version
bump. Choose deliberately when the time comes.

## Reader requirement

A reader MUST verify the `version` field early — before trusting any
other field. On encountering an unknown future version, the reader MUST
emit an error of the form:

```
ptyrecord version <N> not supported by this reader (max supported: <M>); upgrade ptyrecord or write with --bundle-version <M>
```

where `<N>` is the writer version and `<M>` is the reader's
`PTYRECORD_VERSION`. The current implementation in `PtyRecord::validate`
emits exactly this message.

The reader MAY also reject impossibly low versions (e.g. `0`), but the
primary contract is the forward-incompatibility error.

## Writer requirement

A writer MUST always emit the current `PTYRECORD_VERSION` constant.
A writer MUST NEVER hand-write a `version` higher than the constant it
was compiled against — doing so produces a bundle no extant reader can
understand. If a tool exposes a `--bundle-version` flag for emitting
older formats, that flag MUST refuse values higher than the build's
`PTYRECORD_VERSION`.

## Relationship to `.ptytrace`

A `.ptyrecord` bundle **wraps** a `.ptytrace` (the base64-embedded
asciinema v2 cast). The `.ptytrace` format has its own version field
governed by the asciinema v2 specification — see
<https://docs.asciinema.org/manual/asciicast/v2/>. Compatibility of the
inner trace is **not** a `.ptyrecord` concern:

- Bumping `PTYRECORD_VERSION` does not require bumping the trace
  version, and vice versa.
- A `.ptyrecord` reader validates the outer bundle, decodes the inner
  bytes, and hands them to the `.ptytrace` parser, which enforces its
  own version policy.

If the asciinema spec ever introduces a new trace version, that is
handled in `ptytrace`, not here.

## Reader compatibility (`ptyroom play`)

`ptyroom play` is a planned binary that does not yet exist. When it is
built, it SHOULD extension-detect both `.ptytrace` and `.ptyrecord`:

- A `.ptyrecord` is unwrapped to its embedded `.ptytrace` before
  replay. The validation in `PtyRecord::validate` runs first, then the
  inner trace bytes are decoded and replayed.
- A `.ptytrace` is replayed directly, skipping the unwrap step.

This keeps a single user-facing command for both artifact shapes. The
reverse — a `.ptyrecord`-only reader — would force users to repackage
existing `.ptytrace` files before replay, which is unnecessary friction.
A `.ptytrace`-only reader would force every share-link viewer to also
have the media artifact unbundled out-of-band, which defeats the point
of `.ptyrecord` as a self-contained share artifact.

The unwrap path is the same code path as `PtyRecord::validate` followed
by `Trace::parse` on the decoded `trace.bytes_base64` payload.
