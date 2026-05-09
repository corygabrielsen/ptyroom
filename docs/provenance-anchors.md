# Provenance Anchors And Trust Algebra

The `ptyroom` toolchain can prove that media was rendered from a trace.
That is not the same as proving the trace came from a particular machine,
user, service, or courtroom exhibit. A trace is bytes; bytes can be
fabricated.

The missing durable object is an attestation: a provider-specific claim
over the trace digest.

## Objects

```text
S = session, command, or external context
T = .ptytrace bytes
R = render options
M = media bytes, usually GIF or MP4
C = behavioral contract
A = attestation sidecar
W = witness sidecar
h = SHA-256 over exact file bytes
```

Implemented files map directly onto those objects:

```text
T : crates/ptytrace/src/trace.rs
M : crates/ptyrender/src/render.rs + crates/ptyrender/src/encode.rs
C : crates/ptytrace/src/contract.rs
A : crates/ptytrace/src/attestation.rs
W : crates/ptyrender/src/witness.rs
```

## Operations

```text
capture : S -> T
render  : T x R -> M
check   : T x C -> Bool
attest  : Provider x h(T) -> A
witness : h(T) x R x h(M) x h(C)? x h(A)? -> W
verify  : W x T x M x C? x A? -> Outcome
```

The current implementation supports one optional attestation sidecar in
`Witness::attestation_sha256`. The algebra generalizes to multiple
anchors by replacing `A?` with a set of attestations or an aggregate
attestation whose target is still `h(T)`.

## Verification Predicate

For a witness with both a contract and an attestation:

```text
verify(W, T, M, C, A) =
  h(T) == W.trace_sha256
  AND h(M) == W.output_sha256
  AND h(C) == W.contract_sha256
  AND h(A) == W.attestation_sha256
  AND render(T, W.render) == M
  AND check(T, C)
  AND A.target_sha256 == h(T)
  AND verify_provider(A) == Trusted
```

The load-bearing law is:

```text
A.target_sha256 == h(T)
```

Without that law, a verifier can be tricked by a valid attestation for
one session paired with a fabricated trace from another session.

## Three Trust Layers

Keep these separate:

| Layer | Question | Examples | Durable in witness? |
| --- | --- | --- | --- |
| Render witness | Was this media rendered from this trace by this pipeline? | `Witness`, ffmpeg identity, font hash | Yes |
| Behavioral contract | Did this trace contain the expected behavior? | text/color predicates, forbidden output | Yes, by contract hash |
| Provenance anchor | Did some external identity bind itself to this trace hash? | SSH signature, KMS signature, TPM quote, OIDC token, log inclusion | Yes, by attestation hash |

Authenticated transport is useful but not enough by itself. SSH,
WireGuard, TLS, or a private network can control who participates during
a live session. They become durable evidence only when some provider
emits an attestation targeting `h(T)`.

## Provider Interface

The code uses two provider traits:

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

Every provider writes the same provider-independent fields:

```text
kind
issuer
subject
context
target_sha256
freshness
proof
```

`context`, `freshness`, and `proof` are provider-specific. `target_sha256`
is not. The target is what lets unrelated trust mechanisms compose with
the same trace, witness, contract, and media.

## Substitution Rule

SSH is substitutable with another provider exactly when that provider can
make a verifier-checkable claim over `h(T)`.

A provider is a valid substitute for SSH in this topology if it satisfies
all of these laws:

| Law | Requirement |
| --- | --- |
| Target binding | The provider proof covers `A.target_sha256`, and `A.target_sha256 == h(T)`. |
| Provider verification | A verifier for `A.kind` can return `Trusted`, `Invalid`, or `UnsupportedKind` without relying on the witness text. |
| Subject semantics | `issuer`, `subject`, and `context` have provider-defined meaning that a reviewer can interpret. |
| Freshness policy | Replay resistance is explicit: none, nonce, timestamp, or nonce plus timestamp. |
| Byte stability | `h(A)` is over exact serialized attestation bytes, so a witness cannot silently swap sidecars. |

Substitution does not mean all providers prove the same fact. It means
they plug into the same verification equation.

## Provider Matrix

| Provider | What it can prove | What it does not prove |
| --- | --- | --- |
| SSH signature | A user key, host key, or SSH CA principal signed `h(T)`. | The command was authorized, legal, complete, or honestly recorded. |
| SSH transport | The live byte stream crossed an authenticated SSH channel. | Durable proof after the fact unless the channel metadata is signed into `A`. |
| WireGuard / overlay network | A peer key or network identity was allowed on the private transport. | Which human acted, or what happened after bytes entered the PTY. |
| TLS / mTLS | A certificate identity participated in a channel or signed `h(T)`. | That the terminal state was semantically correct. |
| KMS / HSM | A managed key signed `h(T)` under an account, role, policy, or hardware boundary. | That the operator or workload should have been allowed to request the signature. |
| TPM | A device produced a quote binding `h(T)` to machine state or PCRs. | User intent, app correctness, or completeness of capture. |
| CI / OIDC | A workflow identity, repo, commit, run id, or deploy job bound itself to `h(T)`. | That source code was safe, reviewed, or free of supply-chain compromise. |
| Sigstore / Fulcio / Rekor | An OIDC identity signed `h(T)` and optionally logged it. | That the trace is truthful beyond the identity and log guarantees. |
| Transparency log | `h(T)`, `h(W)`, or `h(A)` existed in an append-only log at an index/time. | Who created it unless combined with a signature identity. |
| File provider | A local unsigned sidecar targets `h(T)`. | Any external identity. This is for fixtures, demos, and plumbing tests. |

## Example Topologies

### Remote SSH Session

```text
ptytrace ssh host.example.com -> T
ssh-key-sign(h(T))            -> A_ssh
ptyrender T M --receipt W --attestation A_ssh
verify(W, T, M, A_ssh)
```

The SSH channel made the session possible. The durable claim is the SSH
signature over `h(T)`.

### Shared Terminal Behind SSH

```text
ssh -L 7000:127.0.0.1:7000 host
ptyroom host --listen 127.0.0.1:7000 -> T
provider-sign(h(T))              -> A
```

The tunnel authenticates the live transport. The attestation makes an
after-the-fact claim about the finished trace.

### CI Demonstration

```text
ptytrace run demo.script -> T
OIDC token exchange over h(T) -> A_oidc
contract check(T, C)
render(T, R) -> M
witness(h(T), R, h(M), h(C), h(A_oidc)) -> W
```

The verifier checks reproducibility, behavior, and workload identity
against one trace digest.

### Courtroom Exhibit

```text
forensic source material -> scripted or live reconstruction -> T
expert/lab/court system signs h(T) -> A
render(T) -> M
witness(T, M, A) -> W
```

The GIF or MP4 explains the exhibit. The trace is the replayable record.
The attestation says which expert, lab, or system bound itself to that
record. Chain of custody for the underlying evidence still lives outside
`ptyroom`.

## What Verification Does Not Prove

Even a fully verified tuple `(W, T, M, C, A)` does not prove:

- the trace is a complete record of the world;
- the operator had authority;
- the command was safe;
- the external system was uncompromised;
- the contract captured every relevant behavior;
- legal chain of custody exists.

It proves the narrower and useful claim:

```text
This exact media, behavior claim, and provider claim all meet at this
exact trace digest.
```

That is the algebraic value: independent evidence surfaces compose by
hashing to the same object.
