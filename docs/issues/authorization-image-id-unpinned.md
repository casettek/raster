# Issue: `authorization-image-id-unpinned` — the authorization guest a fraud proof relies on is the prover's choice

Status: **resolved 2026-10-21** with directions 1 and 2 (§Resolution). Opened the same day.
**Soundness gap**, found by reading and not probed. It was flagged in
[`control-flow-prover`](../proposals/control-flow-prover.md) §Image-id pinning ("the pattern
being copied lacks it") and filed here so that step 6 does not copy it.

Related:
- [`control-flow-prover`](../proposals/control-flow-prover.md) step 6 adds a second inner image
  id, `control_flow_image_id`, read and committed the way this one is. Whatever closes this issue
  has to be applied there too.
- [`trace-end-windows`](../proposals/trace-end-windows.md) §Soundness: for a one-item genesis
  window, "which program, which inputs" is pinned only by the authorization journal.
- [`chain-fraud-proof`](../proposals/chain-fraud-proof.md): the chain path does pin the
  authorization guest (§What it is not).

## What happens

Each transition step verifies an authorization journal, against an image id taken from that
step's own input:

```rust
// guests/transition/src/main.rs:45-48
assert!(verify_authorization_journal(
    &input.authorization_journal,
    &input.authorization_image_id
));
```

`verify_authorization_journal` (`checks/io.rs`) is an `env::verify(image_id, journal)`. It proves
that *some* guest with that image id committed that journal. `input.authorization_image_id`
arrives from the host, and two things are missing:

1. **No continuity across the window.** At a `Next` step the guest checks the previous journal's
   transition image id (`fraud_proof.rs:481`), state (`:496`), manifest commitment (`:507`) and
   program commitment (`:518`). It never compares `input.authorization_image_id` with
   `prev_journal.authorization_image_id` (`fraud_proof.rs:245-250`). `commit_journal` writes the
   current step's id (`fraud_proof.rs:833`), so only the final step's id reaches the `Finished`
   journal. Earlier steps can each name a different one.
2. **No relying party pins even the final id.** The only single-program verifier,
   `raster-prover/examples/verify_fraud_proof.rs`, checks the receipt against
   `TRANSITION_GUEST_ID`, the `Finished` state, the refuted commitment and
   `journal.transition_image_id`. It never reads `journal.authorization_image_id`.

The journal carries soundness weight in two places, and both trust whichever guest the id names:

- `checks::entrypoint::verify_genesis_authorization` (called at `Init`, `fraud_proof.rs:207`):
  the entry object at `[]` in the window's initial storage must commit to
  `combined_root(CFS names, journal)`.
- `checks::entrypoint::verify_step` (on the `ProgramStart` step, `fraud_proof.rs:695`):
  `ProgramStart.output_commitment` must equal `combined_root(CFS names, journal)`.

The real authorization guest (`guests/authorization/src/main.rs`) commits
`external_inputs_commitments` parsed from the manifest, together with
`input_manifest_commitment = sha256(manifest)`. So under the real guest, `Established` means
"the entry arguments are the ones manifest `M` lists". A guest the prover compiled can commit
`input_manifest_commitment = M` next to any commitments it likes.

## What it allows

**With gap 2: a forged receipt against an honest trace.** Take a one-item window at genesis,
which `trace-end-windows` made provable. Its only item is `ProgramStart`. A challenger fabricates
a `ProgramStart` that binds inputs `Y` the manifest does not list. Then:

| check | what the challenger supplies | passes because |
| --- | --- | --- |
| `assert_opens_at_genesis` | the genesis state | it is the real genesis state |
| `verify_exec_index`, `verify_sequence_id` | honest values | they are determined by position and CFS |
| `verify_authorization_journal` | a receipt from its own guest committing `{commitments(Y), M}` | it is a real receipt of that guest |
| `entrypoint::verify_step` | `output_commitment = combined_root(names, commitments(Y))` | it is computed from the forged journal |
| `checks::store` | a write of the entry object for `Y` at `[]` | storage is empty at genesis |
| `finalize` | — | the fabricated record diverges from the honest one |

The `Finished` journal names `M`, `Established`, and the attacker's authorization id, which
nothing checks. That is a fraud receipt against an honest commitment.
`trace-end-windows` §Soundness says this window is "exactly as trustworthy as the manifest behind
it". In fact it is only as trustworthy as the authorization guest behind it, and that guest is
not pinned.

**With gap 2 closed and gap 1 open.** A relying party that pins the final id still accepts a
window whose `Init` step used a forged journal. That journal must name `M` (manifest continuity
holds), but it can list commitments `M` does not contain. `verify_genesis_authorization`
then returns `Established` against those commitments. The final step can carry the real journal,
because `ProgramStart` is not in a window that opens after it, and nothing re-checks the entry
object. So `Established` and `input_manifest_commitment = M` in the `Finished` journal no longer
imply that the entry object matches `M`. How much that is worth to an attacker depends on how the
settlement layer reads those two fields. That layer is not built
([`chain-io-commitment`](../proposals/chain-io-commitment.md) §3). The field's meaning is broken
either way.

## What it is not

- Not the chain path. `verify_chain_fraud_receipt` (`raster-prover/src/chain_fraud.rs:73`)
  requires `journal.authorization_image_id == authorization_guest_image_id()` for `Link`
  evidence, and the chain-fraud guest verifies one authorization receipt, not a chain of them.
  This issue is the single-program `TransitionJournal` only.
- Not manifest or program continuity, which both hold (`fraud_proof.rs:507`, `:518`). The gap is
  that a continuous manifest *commitment* says nothing when the guest that committed it is not
  fixed.
- Not the transition image id. That one is checked for continuity (`fraud_proof.rs:489`), and the
  example verifier pins it.

## Directions

Not picked here.

1. **Continuity in the guest.** Add `assert_eq!(input.authorization_image_id,
   prev_journal.authorization_image_id)` to the `Next` branch. This closes gap 1 only. It is
   cheap and needs no format change.
2. **Pin at the relying party.** `verify_fraud_proof` (and any real verifier) requires
   `journal.authorization_image_id == authorization_guest_image_id()`, as the chain path already
   does. With 1, this closes both gaps. The pin lives outside the guest, so every verifier has
   to remember it.
3. **Compile the id into the transition guest.** The guest drops `authorization_image_id` from
   its input and verifies against a constant, so `TRANSITION_GUEST_ID` commits to it. This closes
   both gaps inside the guest. It needs a two-pass guest build or a checked-in constant pinned by
   a test, and the constant is only as stable as guest builds are reproducible across checkouts
   (tile ELFs are not: they embed absolute source paths).
4. **Verify authorization once per window.** Do it at `Init` only, and carry the journal's digest
   forward in `Transition`, as [`transition-split`](../proposals/transition-split.md)'s
   aggregator does. This removes the per-step `env::verify`, and with it the per-step id. It
   still needs 2 or 3 for the one id that remains.

## Reproduction

By reading, from the citations above: `main.rs:45-48`, `fraud_proof.rs:245-250` and `:833`, and
`examples/verify_fraud_proof.rs`.

The test that would show it: build a second guest that commits an arbitrary
`AuthorizationJournal`, then prove a one-item genesis window whose `ProgramStart` binds inputs
the manifest does not list, using that guest's receipt and id. Today `verify_fraud_proof` accepts
the result. After direction 2 (or 3) it must reject it. For gap 1 alone: a two-step window whose
`Init` step uses the forged id and whose second step uses the real one. After direction 1 the
second step must panic.

## Resolution

Directions 1 and 2, with no format change.

- **Gap 1.** `fraud_proof.rs` `assert_authorization_continuity` runs in the `Next` branch beside
  the manifest and program continuity checks. It requires `input.authorization_image_id ==
  prev_journal.authorization_image_id`, so the one id the `Finished` journal carries is the id
  every step verified against. Tests: `tests::authorization_continuity` in the transition guest.
- **Gap 2.** `raster_prover::transition::verify_fraud_proof_receipt` is the single-program
  counterpart of `verify_chain_fraud_receipt`. It verifies the receipt against
  `TRANSITION_GUEST_ID`, requires `Finished`, and requires the journal's `transition_image_id`
  and `authorization_image_id` to be the known-good guests. Which commitment the proof refutes
  stays the caller's check. `examples/verify_fraud_proof.rs` now goes through it. Tests:
  `transition::tests::a_journal_naming_another_authorization_guest_is_refused` and three
  siblings, on the journal half (`check_fraud_proof_journal`).

The transition guest's image id moves, as any guest change does. Verified end to end on
`examples/hello-tiles` in dev mode: `--fraud-step 58` audits, generates a proof, and
`verify_fraud_proof` accepts it with both guests pinned.

Still not done: the reproduction above (a second, forged authorization guest) was not built, so
the attack itself has not been demonstrated. And direction 2 holds only for verifiers that call
`verify_fraud_proof_receipt`. A settlement contract written later has to make the same check.

**Superseded 2026-10-22 by FRAUD WINDOW** (`transition-split` migration steps 4–5). The fraud
proof is now a FRAUD WINDOW receipt, and every inner guest id — the authorization guest's among
them — is compiled into that guest rather than read from the host or a journal. The bug class
can't recur: no image id is an input anywhere in the single-program path, and a relying party
pins one id, `FRAUD_WINDOW_GUEST_ID` (`raster_prover::fraud_window::verify_fraud_window_receipt`).
The continuity asserts, `verify_fraud_proof_receipt` and the transition chain they protected are
deleted.
