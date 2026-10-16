# Issue: `sequence-scope-forbids-narrowing` — a sub-sequence may not `select!` into its own parameter

Status: **resolved 2026-10-08** — §Resolution. Opened 2026-09-21; blocked fraud-proof construction
on `examples/hello-tiles`.
**Re-checked 2026-10-08 at `28474b1`, before the fix: still open**, code unchanged in substance (line references
below refreshed). Since filing it has been the recorded failure of every `hello-tiles` window
that contains a narrowing step: `35:4` in
[`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md)'s
batch A, exec 3 in its batch C, steps 1–32 in its batch E, and windows inside recur sequence `[15]` in
[`tile-io-structural-roots`](../proposals/tile-io-structural-roots.md). The fix direction is
now cheaper than when this was written — see §Directions.

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
  carry it. **Resolved 2026-10-01** by
  [`tile-io-structural-roots`](../proposals/tile-io-structural-roots.md) step 2: each
  storage-bound argument's selection must now fold from its source's root. That removes the
  shared obligation §Directions said both issues needed; this one is now independent.
- [`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md)
  §Implemented so far (2026-09-28) — the same root cause seen from the resolver: *"Argument
  bindings carry no path, so once the walk crosses a sequence parameter only the trailing
  segments are known (`path_complete = false`) … Giving bindings a path is a separate change,
  and also closes `sequence-scope-forbids-narrowing`."*
- [`draft-provenance`](../proposals/draft-provenance.md) — the sibling defect one level up: a
  binding that loses information the resolver actually had. There `finalize(d)` collapses to
  `Inline`; here a narrowed scope parameter collapses to `SequenceScope { input_index }`.

## What happens

`examples/hello-tiles`, tile guests current, fraud injected with the built-in injector and
audited. Fraud is **detected** (the run reaches `prove()`, which a program run reaches only from
the `VerificationResult::Fraud` arm — `run.rs:305` → `generate_fraud_proof`, `prove.rs:70-83`;
its other caller is chain fraud proving, `chain.rs:1330`), and the transition guest then panics
(log as recorded on 2026-09-21; the 2026-10-08 re-run is in §Reproduction):

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
(`crates/raster-core/src/cfs.rs:1023-1025`):

```rust
SequenceScope {
    input_index: usize,
},
```

So the CFS records *"this argument comes from the enclosing sequence's parameter N"* and has no
way to record *"…narrowed by selector path P"*. Given that binding, the guest's reading is the
only one available (`checks/cfs.rs:808-822`):

```rust
InputBinding::SequenceScope { input_index } => {
    let sequence_scope_witness = /* … */;
    let scope_source = if *input_index == 0
        && is_recur_sequence_iteration_frame(cfs_cursor, parent_sequence_coordinates)
    {
        recur_sequence_item_source(sequence_scope_witness)
    } else {
        resolved_source_at(sequence_scope_witness, *input_index)
    };
    assert_same_source(resolved_source, scope_source);   // :822
}
```

and `assert_same_source` (`:123-141`) is `assert_eq!` on the whole `StorageData`. The recur-sequence
branch (added since filing) changes where the scope value comes from, not how it is compared, so
a recur-sequence body that narrows its item fails the same way. The defect is therefore **not**
that the assertion is wrong for what the binding claims; it is that the binding cannot state what
the program did.

The host-side resolver has the same blind spot. `CfsCursor::resolve_value` (`cfs.rs:368-371`,
`:441`) sets `path_complete = false` on crossing a `SequenceScope`, because the argument binding
carries no path either. Its consumers weaken rather than fail: `ProgramEnd`
(`checks/entrypoint.rs:357-367`) accepts any output selector that merely *ends with* the known
suffix, and prior-item arguments get no path check at all.

## Reproduction

**Deterministic as of 2026-09-21**, via `--fraud-step`:

```console
$ cd examples/hello-tiles
$ cargo raster run --input input.json --input-manifest input_manifest.json \
    --commit probe45.bin --fraud-proof-window-size 32 --fraud-step 45
$ cargo raster run --input input.json --input-manifest input_manifest.json --audit probe45.bin
```

Index 45 was `exec:75`, `personal_greet` at `[21, 1]` — the narrowing call site from `main.rs:25`.
`--fraud-step list` printed 58 eligible steps. The flag enables the injector on its own, so no
`fraud_` filename prefix is needed.

> **Index 45 is stale as of 2026-10-08.** The program has grown to **64** eligible steps and index
> 45 is now `decorate_address_line` at `[21, 1, 1]`. `personal_greet` — the tile behind the
> narrowing `select!` at `main.rs:25`, reached through `greet_sequence` from both `main.rs:67` and
> `main.rs:197` — is now index **0** (`exec:3`, `[1, 1]`) and index **51** (`exec:86`, `[23, 1]`).
> Indices move whenever `hello-tiles` changes; re-run `--fraud-step list` before trusting one.

Re-run 2026-10-08 at `28474b1`, `RISC0_DEV_MODE=1` (the transition guest executes; receipts are
dev receipts), **50 s** commit + audit:

```console
$ cd examples/hello-tiles
$ RISC0_DEV_MODE=1 cargo raster run --input input.json --input-manifest input_manifest.json \
    --commit probe0.bin --fraud-proof-window-size 32 --fraud-step 0
$ RISC0_DEV_MODE=1 cargo raster run --input input.json --input-manifest input_manifest.json \
    --fraud-proof-window-size 32 --audit probe0.bin
thread 'main' (1) panicked at src/checks/cfs.rs:132:13:
assertion `left == right` failed: Storage sequence scope input does not match consumer binding
  left:  StorageData { coordinates: CfsCoordinates([]),
           selector: SelectorPath { segments: [Field("personal_data"), Field("name")] },
           selection: SelectionCommitment { ..., selected_len: 21,  payload_kind: Raw } }
  right: StorageData { coordinates: CfsCoordinates([]),
           selector: SelectorPath { segments: [Field("personal_data")] },
           selection: SelectionCommitment { ..., selected_len: 694, payload_kind: Raw } }
```

Same source root (`6e799526…`) and the same 21-vs-694 lengths as the original. The selector reads
`personal_data` rather than `personal_data_bin` because this call is `main.rs:67`'s, not
`:197`'s. Audit exits `101`. Use the dev-mode run as the gate; the 2.5 h figure in the **Cost**
note below is for real proving.

> Beware `tile:personal_greet`: that tile runs at **two** call sites, and `tile:` resolves to the
> first. Use the index or `exec:` form here.

Originally found with the unseeded injector, where the `fraud_` prefix was load-bearing: the gate
was `commit_path.starts_with("fraud_")` alone (now `run.rs:294`, `|| fraud_step.is_some()`), a
literal path-prefix test, so an absolute path to a file *named* `fraud_demo.bin` did **not**
trigger it and the audit returned `Verification Success` with exit `0` — a false green that cost
one probe to diagnose.

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
2. **`main`'s own arguments are `EntryArgument`, not `SequenceScope`** (`cfs.rs:1027-1034`), so a
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
   > knowable without guessing. *(Known 2026-10-08: zero on hello-tiles in dev mode once this
   > issue was fixed — §Resolution's 64-step sweep.)*
   >
   > **Update 2026-10-08.** The sibling, `authenticated-chain-draft-output`, was resolved
   > 2026-10-01 (draft batch B), and the dev-mode windows recorded in
   > `incremental-draft-materialization` and `tile-io-structural-roots` that avoid a narrowing step
   > now prove. The latest of them (batches C and E, 2026-10-01) record this issue as the only
   > remaining failure on `hello-tiles`. The step count is now 64 (see §Reproduction).

## Directions

Sketched, not chosen.

- **Widen the binding.** Give `SequenceScope` a selector path — the narrowing the resolver
  already sees at `select!` time — and have the guest check that the consumer's path extends the
  scope's on the same source root, plus that the consumer's `SelectionCommitment` verifies
  against the scope's selected value as its source. Most faithful; the last clause is the real
  work and is the same selection-composition obligation
  [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) needs, so the two
  should be looked at together rather than solved twice.

  > **Update 2026-10-08 — the last clause is now mostly paid for.** `selection-unbound-from-execution`
  > was resolved 2026-10-01: every storage-bound argument's selection must fold from its source's
  > root (`tile-io-structural-roots` step 2). With the narrowing `P` recorded in the CFS (and so
  > hashed into program identity), the guest check becomes: same `coordinates`, same
  > `source_root_hash`, and consumer selector **exactly** `scope selector ++ P`, not merely
  > some extension of it. The existing fold then means the consumer's value is the `P`-sub-value
  > of the scope's value, with no separate composition proof. Still to check: that this holds
  > across `payload_kind`s and through `Indexed` bindings, where `P` contains data-sourced
  > indexes. Two more consequences:
  >
  > - **One fix, two consumers.** The same path lets `resolve_value` keep `path_complete = true`
  >   across a parameter, which turns `ProgramEnd`'s suffix match back into an exact one and
  >   gives prior-item arguments a path check.
  > - **Identity break.** `InputBinding` is postcard-encoded into committed schemas. Adding a
  >   field to `SequenceScope` changes that variant's encoding for every program, and `Indexed`
  >   was declared last precisely to avoid such shifts. Either accept the break (and bundle it
  >   with the next one) or add a new trailing variant, e.g. `SequenceScopeSelected { input_index,
  >   path }`, and leave `SequenceScope` for the un-narrowed case.
- **Relax the guest check to prefix-extension only.** Compare `coordinates` and
  `source_root_hash`, require the consumer's selector to extend the scope's, and stop there. Small
  and unblocks the path — but it accepts any selection under the scope's root without proving the
  narrowing was performed on the scope's *value*, which is precisely the join this family of
  issues keeps failing to make. It should be taken as a deliberate, recorded weakening if taken
  at all, not as the fix.
- **Make the injector targetable.** *Done 2026-09-21* (`--fraud-step`). Not a fix for this
  defect, but a precondition for knowing whether it is the last one. A `--fraud-step` selector (or seeding the RNG and recording the
  seed) turns a 2.5 h coin flip into a repeatable gate, and lets `authenticated-chain-draft-output`
  and this issue each have a reproducer that runs on purpose.

## Cost of leaving it

No fraud proof can be completed for any program in which a sub-sequence narrows one of its own
parameters — which the authoring guidance tells every program to do. Combined with
`authenticated-chain-draft-output`, the position on `hello-tiles` was that divergence is reliably
*detected* and a receipt has never once been *built*. As of 2026-10-08 that sibling is resolved
and dev-mode receipts are built for windows that avoid a narrowing step. Any window that contains
`personal_greet` at `[1, 1]` or `[23, 1]` still cannot be proven, and an honest party cannot
choose which step a cheater corrupts. On the dispute path a completeness defect is
a lost dispute, the framing
[`fraud-evidence-storage-unavailable`](./fraud-evidence-storage-unavailable.md) §3 uses.

## Resolution

**Resolved 2026-10-08** by the first direction above, widened to every argument binding: each
value variant carries the path its argument reads.

```rust
SequenceScope { input_index, path: Vec<SelectorSegment> },
PriorItemOutput { intra_sequence_item_index, path: Vec<SelectorSegment> },
EntryArgument { path: Vec<SelectorSegment> },   // starts with the argument's name
```

`path` is what the argument appends to the selector of the value it names. That is the convention
`SequenceReturn::path` already used, and it is how the runtime builds the selector. Measured on
hello-tiles before any guest change: all 78 storage-backed arguments recorded exactly
`base ++ path`.

- **Compiler.** `CallArgumentKind::Rooted` carries the argument's own static `select!` path
  (`ast.rs::argument_path`, reusing `selection_macro_path`). `FlowResolver::resolve_argument`
  prefixes the alias chain (`compose_alias_path`) and, for an entry argument, the argument's
  name. `resolve_return` keeps a path-less source, so the return path is not counted twice.
- **Resolver.** `CfsCursor::resolve_value` prepends each binding's path at every hop. A walk
  through a parameter now knows the whole selector, so `ResolvedValue::path_complete` is gone.
- **Guest** (`checks/cfs.rs::verify_one_binding`, every check on `selection.path`, the path the
  selection proof is pinned to):
  - **scope:** same coordinates, commitment and source root as the caller's argument, and
    exactly `caller.selection.path ++ path` (`assert_scope_source`);
  - **entry argument:** exactly `path`;
  - **prior item:** exactly the resolved path, or `ends_with(path)` on the existing fallback for
    an unbound return;
  - **`ProgramEnd`:** exact. Its suffix-only branch is removed.

  The last three had no selector check at all before. Any part of the entry object, or any
  field of a producer's output, passed.
- **Identity break.** Every program's `program_commitment` moves (the encoding of three
  variants changed), and so does the transition image id. All six tracked `Raster.lock` files
  were regenerated with the workspace CLI. hello-tiles is now `1d75afea…`.

**Verified.**
- **Unit tests.** `raster-core` 180 (1 new), `raster-compiler` 48 (2 new: argument paths compose like
  the runtime selector; an indexed argument records no path but its supplier does), transition
  guest 159. The 12 new guest tests cover:
  - a narrowed scope parameter, accepted, plus four rejections: a sibling field, a declared
    narrowing the step skipped, an undeclared one, and another object;
  - narrowing an inline parameter, which fails closed;
  - entry-argument and prior-item selectors, accept and reject each;
  - a `ProgramEnd` through a narrowing pass-through, accept and reject (the reject passed under
    the old suffix rule).
- **hello-tiles.**
  - Honest commit and audit: `Verification Success`, exit 0. `program --verify` matches the lock.
  - `RISC0_DEV_MODE=1 --fraud-step 0` (`[1, 1]`, 49 s) and `--fraud-step 51` (`[23, 1]`, 158 s):
    `Fraud proof generated`, exit 0.
  - Both receipts verify (`verify_fraud_proof`): journal `Finished`, the refuted commitment
    matches each `commit.bin`, program commitment `1d75afea…`.
  - **Sweep: all 64 eligible steps** (`--fraud-step 0..=63`, dev mode, window 32). Every one
    produces a fraud proof, and every receipt verifies on the same four checks: image id,
    `Finished`, its own refuted commitment, `1d75afea…`. Each takes 52–168 s and levels off at
    about 160 s once the window is full; the whole sweep took about 2.5 h. This answers
    §Why it was invisible's open count: on hello-tiles, in dev mode, **no blocker remains on
    the fraud path**.
- **Workspace.** `cargo test --workspace --no-fail-fast`: 0 failures, including the CPU proving
  tests and the chain, recur and show CLI suites.

**Limits, recorded.**
- **Indexed arguments.** An argument reached through a data-sourced index (`Indexed`) has no
  static path. It records none and is held only to its source's prefix; its index values are
  checked by their citations, as before.
- **Inline parameters.** Narrowing an *inline* scope parameter fails closed. There is no
  selector over inline bytes in the guest.
- **Real proving.** GPU and CPU proving of real receipts were not rerun; dev mode runs the same
  guest.
