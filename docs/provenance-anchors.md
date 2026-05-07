# Provenance Anchors

`ptytrace` currently proves a render claim:

```text
this media was rendered from this trace by this render pipeline
```

That is valuable, but it does not prove the trace came from a real
session. A PTY trace is just bytes. If someone fabricates the bytes,
the current witness can still faithfully certify the fabricated story.

The missing object is a provenance anchor: a verifiable claim that an
identity, machine, service, or session bound itself to the trace digest.

## Current Topology

```text
Script -------------------------- run --------------------------> Trace
src/script/exec.rs                                              src/trace.rs

Live PTY session ---------------- capture ----------------------> Trace
src/pty/live.rs                                              src/trace.rs

Trace --------------------------- render -----------------------> Media
src/render.rs                                                   GIF / MP4

Trace × Media × RenderOptions ---- witness ---------------------> Witness
src/render.rs                                                   src/witness.rs

Trace × Contract ---------------- check ------------------------> Bool
src/contract.rs

Witness × Trace × Media × Contract -- verify -------------------> Bool
src/witness.rs
```

The current `Witness` contains:

- `trace_sha256`
- render configuration
- tool identity
- `output_sha256`
- optional `contract_sha256`
- optional `script_sha256`
- optional `attestation_sha256`

`script_sha256` records the recipe for scripted recordings. It is useful
provenance, but it is not an attestation that the trace happened in a
particular external context. Verification does not re-run the script,
and live captures have no script recipe at all.

## Target Topology

Add one new object:

```text
A = Attestation
```

Then the topology becomes:

```text
Session/Context ---------------- capture -----------------------> Trace
       |                                                           |
       | attest(hash(Trace))                                      | hash
       v                                                           v
Attestation -------------------------------------------------- target_sha256

Trace --------------------------- render -----------------------> Media
Trace × Contract ---------------- check ------------------------> Bool

Trace × Media × Contract × Attestation -- witness --------------> Witness
Witness × Trace × Media × Contract × Attestation -- verify -----> Bool
```

The witness should not claim "SSH happened" by embedding SSH-looking
strings. It should commit to an `Attestation` object that can be verified
independently and whose target is the trace hash.

## Algebra

Objects:

```text
S = session or external context
T = trace
M = media
C = contract
A = attestation
W = witness
```

Operations:

```text
capture : S -> T
render  : T -> M
check   : T × C -> Bool
attest  : Provider × hash(T) -> A
witness : T × M × C × A -> W
verify  : W × T × M × C × A -> Bool
```

Verification predicate:

```text
verify(W, T, M, C, A) =
  hash(T) == W.trace_sha256
  AND hash(M) == W.output_sha256
  AND hash(C) == W.contract_sha256, when present
  AND hash(A) == W.attestation_sha256, when present
  AND render(T, W.render) == M
  AND check(T, C), when present
  AND verify_attestation(A)
  AND A.target_sha256 == hash(T)
```

The important law is:

```text
A.target_sha256 == hash(T)
```

Without that law, an attacker can pair a real attestation from one
session with a fabricated trace from another session. The attestation
would be valid, but irrelevant.

## Substitution Rule

SSH is one possible anchor provider. It is substitutable with any
provider that can emit a verifiable claim over `hash(T)`.

Minimum provider interface:

```rust
trait AttestationProvider {
    fn kind(&self) -> &'static str;
    fn attest(&self, trace_sha256: &str) -> anyhow::Result<Attestation>;
}

trait AttestationVerifier {
    fn kind(&self) -> &'static str;
    fn verify(&self, attestation: &Attestation) -> anyhow::Result<AttestationOutcome>;
}
```

Minimum attestation shape:

```rust
struct Attestation {
    version: u32,
    kind: String,
    issuer: String,
    subject: String,
    context: serde_json::Value,
    target_sha256: String,
    freshness: Freshness,
    proof: serde_json::Value,
}
```

`context` is provider-specific. `target_sha256` is not.

## Candidate Providers

SSH:

```text
subject: user key fingerprint or user cert principal
issuer: host key fingerprint or SSH CA
context: remote user, host, session id when available
proof: signature or transcript-derived binding over hash(T)
```

KMS / HSM:

```text
subject: key id
issuer: cloud account or HSM cluster
context: account, region, role, key policy snapshot if available
proof: KMS signature over hash(T)
```

TPM:

```text
subject: device identity
issuer: TPM endorsement / attestation chain
context: PCR set, boot state, machine identity
proof: quote with hash(T) as nonce or qualifying data
```

CI / OIDC:

```text
subject: repo, workflow, run id, commit
issuer: OIDC issuer
context: job, actor, ref, workflow sha
proof: token or exchange result that binds to hash(T)
```

Transparency log:

```text
subject: witness hash
issuer: log identity
context: log index, tree size, timestamp
proof: inclusion proof for hash(W)
```

## Code Insertion Points

Add `src/attestation.rs`:

```text
Attestation
AttestationRef / attestation_sha256
AttestationProvider
AttestationVerifier
AttestationOutcome
```

Extend `src/witness.rs`:

```text
Witness {
    ...
    attestation_sha256: Option<String>,
}

VerifyOutcome {
    ...
    AttestationRequired,
    AttestationDiffers,
    AttestationTargetDiffers,
}
```

Extend `src/render.rs`:

```text
Render {
    ...
    attestation_sha256: Option<String>,
}

Render::attestation_sha256(hash)
```

Extend CLI surface:

```text
ptytrace attest file --trace T --out A
ptyrender TRACE OUT --receipt W --attestation A
ptyrender TRACE OUT --receipt W --attestation-out A
ptytrace run SCRIPT --out OUT --receipt W --attestation A
ptytrace run SCRIPT --out OUT --receipt W --attestation-out A
ptytrace verify --witness W --trace T --attestation A
```

The first implementation is detached:

```text
1. Produce trace.
2. Produce or load attestation over hash(trace).
3. Render trace to media and include hash(attestation bytes) in witness.
4. Verify witness, trace, media, optional contract, and attestation together.
```

This avoids coupling recorder internals to SSH/KMS/TPM on day one.
The built-in `file` provider is explicitly unsigned. It proves the
sidecar binds to the trace digest; it does not prove an external
identity.

## Threat Boundary

An attestation does not prove the whole world was true. It proves only
that the named provider validated a proof whose target was the trace
hash.

Examples:

- SSH host/user identity does not prove authorization or legality.
- KMS signature does not prove the trace was complete.
- TPM quote does not prove the operator intended the action.
- Transparency-log inclusion does not prove the trace was honest.

Those are separate contracts or external evidence. The invariant here
is narrower and stronger:

```text
media, behavior, and provenance all meet at the same trace digest
```
