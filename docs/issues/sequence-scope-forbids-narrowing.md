# Issue: `sequence-scope-forbids-narrowing` — a sub-sequence may not `select!` into its own parameter

Status: open 2026-09-21. Unowned. Blocks fraud-proof construction on `examples/hello-tiles`.

Related:
- [`authenticated-chain-draft-output`](./authenticated-chain-draft-output.md) — until today this
  was recorded as *"the **last** blocker standing between a detected divergence and a completed
  fraud proof on `hello-tiles`: every other step in that window verifies."* **That is no longer
  accurate**, and the reason it read that way is in §Why it was invisible below.
- [`fraud-evidence-storage-unavailable`](./fraud-evidence-storage-unavailable.md) — same family:
  fraud is detected and the receipt cannot be built. Different assertion, different cause; the
  shared shape is that the honest path returns `Ok` before `prove()` and so never runs the check.
- [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) — adjacent and, in
  the abstract, the same missing join: a selection that is verified and a consumer that is
  verified, with nothing relating them. Here the relation is *expressible* but the CFS does not
  carry it.
- [`draft-provenance`](../proposals/draft-provenance.md) — the sibling defect one level up: a
  binding that loses information the resolver actually had. There `finalize(d)` collapses to
  `Inline`; here a narrowed scope parameter collapses to `SequenceScope { input_index }`.

## What happens

`examples/hello-tiles`, tile guests current, fraud injected with the built-in injector and
audited. Fraud is **detected** (the run reaches `prove()`, which has one call site — `run.rs:323`,
inside the `VerificationResult::Fraud` arm), and the transition guest then panics:

```
thread 'main' (1) panicked at src/checks/cfs.rs:59:13:
assertion `left == right` failed: Storage sequence scope input does not match consumer binding
  left:  StorageData { coordinates: CfsCoordinates([]),
           selector: SelectorPath { segments: [Field("personal_data_bin"), Field("name")] },
           selection: SelectionCommitment { ..., selected_len: 21,  payload_kind: Raw } }
  right: StorageData { coordinates: CfsCoordinates([]),
           selector: SelectorPath { segments: [Field("personal_data_bin")] },
           selection: SelectionCommitment { ..., selected_len: 694, payload_kind: Raw } }
```

`commitment` and `selection.source_root_hash` are **byte-identical** on both sides
(`6e79952633d0417eb6f7a71bb4714230a976e67c71e0fa58e57428dd79aa7878`). Only the selector differs,
and the left is a strict **extension** of the right: the consumer narrowed the scope value by one
field. `selected_len` 694 → 21 is the whole `PersonalData` against its `name`.

The audit exits `101`.

## The program shape that triggers it — which the authoring rules recommend

`examples/hello-tiles/src/main.rs`:

```rust
#[sequence]
fn greet_sequence(name: String, personal_data: PersonalData) -> String {
    call!(personal_greet, select!(String, personal_data.name));   // :25  — narrows
    ...
}

#[sequence]
fn main(...) {
    call_seq!(greet_sequence, "Rust".to_string(), personal_data.clone());   // :67 — whole object
}
```

The caller passes the whole object; the callee selects the one field its tile needs. That is not
an unusual shape — the authoring skill's §2 states the rule directly: *"select the smallest
sub-value a tile actually needs — the field, not the struct; never pass a whole object where one
field suffices."* The guest check forbids exactly what the guidance requires.

## Why the check cannot currently pass

`InputBinding::SequenceScope` carries an index and nothing else
(`crates/raster-core/src/cfs.rs:827-829`):

```rust
SequenceScope {
    input_index: usize,
},
```

So the CFS records *"this argument comes from the enclosing sequence's parameter N"* and has no
way to record *"…narrowed by selector path P"*. Given that binding, the guest's reading is the
only one available (`checks/cfs.rs:698-706`):

```rust
InputBinding::SequenceScope { input_index } => {
    let scope_source = resolved_source_at(sequence_scope_witness, *input_index);
    assert_same_source(resolved_source, scope_source);   // :706
}
```

and `assert_same_source` (`:50-67`) is `assert_eq!` on the whole `StorageData`. The defect is
therefore **not** that the assertion is wrong for what the binding claims; it is that the binding
cannot state what the program did.

## Reproduction

**Deterministic as of 2026-09-21**, via `--fraud-step`:

```console
$ cd examples/hello-tiles
$ cargo raster run --input input.json --input-manifest input_manifest.json \
    --commit probe45.bin --fraud-proof-window-size 32 --fraud-step 45
$ cargo raster run --input input.json --input-manifest input_manifest.json --audit probe45.bin
```

Index 45 is `exec:75`, `personal_greet` at `[21, 1]` — the narrowing call site from `main.rs:25`.
`--fraud-step list` prints all 58 eligible steps. The flag enables the injector on its own, so no
`fraud_` filename prefix is needed.

> Beware `tile:personal_greet`: that tile runs at **two** call sites (index 0 at `[1, 1]` and
> index 45 at `[21, 1]`), and `tile:` resolves to the first. Use the index or `exec:` form here.

Originally found with the unseeded injector, where the `fraud_` prefix was load-bearing: the gate
is `commit_path.starts_with("fraud_")` (`run.rs:307`), a literal path-prefix test, so an absolute
path to a file *named* `fraud_demo.bin` did **not** trigger it and the audit returned
`Verification Success` with exit `0` — a false green that cost one probe to diagnose.

**Cost.** Reaching the panic took **2 h 30 m** at ~19 cores and 7.3 GB RSS, no `RISC0_DEV_MODE`.
The panic is inside the guest, so it arrives only after the window's steps have been proven —
unlike `fraud-evidence-storage-unavailable`, whose panic is host-side witness building and
returns in seconds. The `17.9 s` figure in that issue is time-to-panic and should not be read as
the cost of a fraud proof.

## Why it was invisible

Three things, and the third is the one worth fixing first.

1. **The honest path never runs this check.** `prove()` is reachable only from the `Fraud` arm,
   so `hello-tiles` commits and audits clean (`Verification Success`, exit `0`) with the defect
   present. This is the same hiding pattern as every other entry in this family.
2. **`main`'s own arguments are `EntryArgument`, not `SequenceScope`** (`cfs.rs:833-838`), so a
   single-sequence program cannot reach the check at all. It needs a `call_seq!` whose callee
   narrows a parameter.
3. **The injector corrupted a *randomly chosen* executed step**, so which checks a window
   exercised differed run to run. `authenticated-chain-draft-output` was recorded as the last
   blocker on the strength of a run that drew a draft-dependent step; this issue was found by a
   run that drew a different one. Neither established what the next would hit.

   > **Fixed 2026-09-21.** `--fraud-step` makes the choice explicit and the default choice
   > seeded-and-reported. `--fraud-step list` shows `hello-tiles` has **58** eligible steps — the
   > space those two probes were sampling blindly. Both known blockers now have named reproducers
   > (`22` here's sibling, `45` this one), and the remaining 56 steps are enumerable rather than
   > hypothetical. **The count of blockers on the fraud path is still unknown**; it is now
   > knowable without guessing.

## Directions

Sketched, not chosen.

- **Widen the binding.** Give `SequenceScope` a selector path — the narrowing the resolver
  already sees at `select!` time — and have the guest check that the consumer's path extends the
  scope's on the same source root, plus that the consumer's `SelectionCommitment` verifies
  against the scope's selected value as its source. Most faithful; the last clause is the real
  work and is the same selection-composition obligation
  [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) needs, so the two
  should be looked at together rather than solved twice.
- **Relax the guest check to prefix-extension only.** Compare `coordinates` and
  `source_root_hash`, require the consumer's selector to extend the scope's, and stop there. Small
  and unblocks the path — but it accepts any selection under the scope's root without proving the
  narrowing was performed on the scope's *value*, which is precisely the join this family of
  issues keeps failing to make. It should be taken as a deliberate, recorded weakening if taken
  at all, not as the fix.
- **Make the injector targetable.** Not a fix for this defect, but a precondition for knowing
  whether it is the last one. A `--fraud-step` selector (or seeding the RNG and recording the
  seed) turns a 2.5 h coin flip into a repeatable gate, and lets `authenticated-chain-draft-output`
  and this issue each have a reproducer that runs on purpose.

## Cost of leaving it

No fraud proof can be completed for any program in which a sub-sequence narrows one of its own
parameters — which the authoring guidance tells every program to do. Combined with
`authenticated-chain-draft-output`, the position on `hello-tiles` is that divergence is reliably
*detected* and a receipt has never once been *built*. On the dispute path a completeness defect is
a lost dispute, the framing
[`fraud-evidence-storage-unavailable`](./fraud-evidence-storage-unavailable.md) §3 uses.
