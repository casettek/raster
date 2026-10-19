# Proposal: `transition-split` — one prover per check, and a transition guest that only aggregates

Status: **Proposed 2026-10-15.** Not measured. §Cost says what has to be measured before
implementation, and step 0 of §Migration measures it.

Related:
- [`chain-fraud-proof`](./chain-fraud-proof.md): consumes the `Finished` `TransitionJournal`. This
  proposal keeps that journal's shape, so the chain-fraud guest changes only in the image id it is
  given.
- [`program-identity`](./program-identity.md): `program_commitment` and the tile registry. Every
  part derives the commitment from the same frame bytes, and the aggregator equates them.
- [`window-seed-reconstruction`](./window-seed-reconstruction.md),
  [`recur-progress-commitment`](./recur-progress-commitment.md): the recur seed rules move into the
  `recur` part unchanged.
- [`storage-write-cost`](./storage-write-cost.md): source of the per-step cycle figure (~2.4M user
  cycles per step, after the framed-input and `EMPTY_ROOTS` fixes) used in §Cost.

## Problem

The transition guest (`crates/raster-prover/guests/transition`) proves a fraud-proof window **one
step per guest execution**. Each execution runs every check family against one shared,
in-memory state (`LiveTransition::apply_verified_step`, `fraud_proof.rs:637`):

```
step k ──► [ exec_index | sequence_id | scope parent | step inputs | drafts | recur progress |
             entrypoint | program end | io + replay | storage | next coords | trace append ]
        ──► finalize (fingerprint compare) ──► TransitionJournal(Next | Finished)
                                                      │
step k+1 ◄── env::verify(prev journal, own image id) ◄┘
```

This has five costs, and none of them is a correctness problem:

1. **Strictly sequential proving.** Step `k+1` takes step `k`'s receipt as an assumption
   (`src/transition.rs:268-292`), so a 32-step window is 32 proofs in a chain. Nothing in the
   window can be proven in parallel.
2. **Per-step fixed overhead that has nothing to do with the step.** Every execution re-reads and
   re-decodes `program.bin` and rebuilds the `CfsCursor` (`PublicParams::read`, `fraud_proof.rs:59`).
   It also `env::verify`s the authorization journal (`main.rs`) and `env::verify`s the whole
   previous `TransitionJournal`, including its fingerprint (`fraud_proof.rs:479`). On top of that,
   each step is a separate receipt that has to be lifted and has its assumptions resolved in
   recursion.
3. **Every step reads every witness.** `TransitionInput` carries every check's witnesses: storage
   reads, selections, the draft pre-state, scope witnesses, replay journals and output bytes. They
   are all deserialized into one heap, even though each check needs only its own slice. The 106 MB
   `List<MergeBucket>` that exhausted the guest heap (`checks/store.rs`, comment at the top of
   `verify_storage_transition`) is an example of this shared heap failing.
4. **The checks depend on each other only implicitly.** `checks::store` mutates the storage
   frontier, and `checks::cfs::advance_recur_progress` and `checks::entrypoint::verify_program_end`
   read it. `checks::io` authenticates the replay journal, and `cfs`, `drafts` and `store` use it.
   The trace frontier is read by `verify_sequence_scope_parent`. All of these hand-offs are local
   variables in one function. The only record of which check relies on which other check is the
   order of the calls.
5. **Everything is reviewed and tested as one guest.** `tests.rs` is 5,535 lines against one entry
   point. A change to the storage check has to be read in the context of all 3,000 lines of checks.

## Goal

Split the transition guest into **part provers**. Each part runs exactly one check family over the
**whole window**, and the transition guest becomes an **aggregator** that verifies the part
receipts, checks that they agree with each other, and commits the same `TransitionJournal` that
the chain-fraud guest already consumes.

Non-goals:
- **No change to any check's logic.** The functions in `checks/*` move between files without
  edits. The only changes are their signatures, where shared mutable state becomes an explicit
  argument (see §Columns).
- **No change to the trace, `commit.bin`, `StepRecord`, the fingerprint, or the `Finished`
  journal.** A fraud proof refutes the same commitment for the same reason.
- No change to the tile replay guests or the authorization guest.

## Design

### Parts

Each part is a separate guest. It reads the window's public parameters once, then streams the
window's steps and keeps only the state it owns.

| part | checks it runs (moved from) | state it owns across the window | witnesses only it reads |
| --- | --- | --- | --- |
| `trace` | `verify_exec_index` (cfs.rs), `append_to_trace`, `finalize`, `assert_window_is_commitment_slice`, `assert_opens_at_genesis` (fraud_proof.rs) | trace frontier, fingerprint accumulator | `TraceCommitmentHeader`, `FingerprintSliceWitness`, revealed tail roots |
| `control_flow` | `verify_sequence_id`, `verify_sequence_scope_parent`, `verify_step_record_inputs`, `get_next_expected_coordinates` (cfs.rs) | `next_expected_coordinates` | `sequence_scope_witness`, `input_sources_witnesses` |
| `recur` | `advance_recur_progress` and its helpers, about lines 1091-1662 of cfs.rs; `assert_seed_present_for_mid_loop_open` (fraud_proof.rs) | `RecurProgressStack` | `window_start_recur_progress`, `returned_state_read_witness` |
| `io` | `verify_io_witness`, `verify_step_record`'s replay half, `verify_output_root` (io.rs) | — | replay journals and replay receipts as assumptions, `input_witness` |
| `store` | `verify_storage_transition`, `verify_input_roots` (moved here from io.rs, see §Columns) | storage frontier, coordinate-index root | `storage_witness`, `storage_selection_witnesses` |
| `drafts` | `verify_draft_transition` (drafts.rs) | — | `draft_transition_witness` |
| `entrypoint` | `verify_genesis_authorization`, `verify_step`, `verify_program_end` (entrypoint.rs) | `EntrypointAuthorization`, `OutputAuthorization` | `entrypoint_membership_witness`, `program_output_*_witness` |

`recur` is split out of `control_flow` because it is the only control-flow check that depends on storage
roots and draft steps. Without it, `control_flow` is pure CFS-conformance checking that depends only on the
trace root. Merging the two back is a one-line change to the part table if the extra receipt turns
out not to pay for itself.

### Columns: every fact that crosses a part boundary has one owner

The split is only sound if each value that a check used to read from another check's state still
comes from that other check's proof. Each such value becomes a **column**. A column holds one
value per window step, and exactly one part **owns** it, meaning that part computes it and proves
it. Every other part that needs the column **consumes** it: the host supplies the values,
unverified, and the consumer hashes the complete column into a digest. The consumer part commits
the digest it consumed, the owner part commits the digest it produced, and the aggregator asserts
that the two are equal. If the host supplied a wrong value at any step, the consumer's digest
differs from the owner's and the aggregator rejects the proof.

| column | value at step `i` | owner (why it is true) | consumers (the call that reads it) |
| --- | --- | --- | --- |
| `trace_root_before` | `frontier_root` before step `i` is appended | `trace` | `control_flow`: `verify_sequence_scope_parent` and the `input_sources_witnesses` folds |
| `storage_roots_before` | (append-log root, index root) before step `i` | `store` | `recur`: the returned-state read in `advance_recur_progress`; `entrypoint`: `verify_program_end` |
| `replay` | structured digest of the `TileReplayJournal`, or none | `io`: `env::verify` against the registry image id | `control_flow`: `verify_step_record_inputs`; `recur`; `drafts`; `store`: `input_roots` |
| `draft_step` | `Option<DraftStep>` | `drafts` | `recur`: `advance_draft` |
| `selection` | digest of the step's proven `storage_selection_witnesses` | `store` | `recur`: `recur_item_selection` |
| `auth_journal` | constant for the whole window | **aggregator**: one `env::verify` of the authorization receipt | `entrypoint` |

The rules that decide what becomes a column:

1. **Witnesses anchored in the record are re-checked locally, not shared as columns.** If a
   witness is bound to the `StepRecord` by one hash, any part that reads it re-checks that hash
   itself. Examples are `input_source_witness` against `input_source_commitment`, and a
   non-execution step's I/O bytes against the record's commitments. This repeats a hash in several
   parts instead of adding a column and an equality check.
2. **Anything that needs state or recursion to prove is a column.** Roots depend on all earlier
   steps, and the replay journal's authenticity depends on an `env::verify`. A consumer part cannot
   re-check either one locally.
3. **Watch for witnesses that look record-anchored but are not.** An *execution* step's
   `output_witness` is not bound by the record: `verify_io_witness` returns early for execution
   steps (`io.rs:37`). It is bound only through `replay.output_bytes`. A part that reads it (for
   example `recur`) must compare it against the `replay` column. This is the main risk in the
   whole proposal: a dependency that nobody maps to a column turns into a host-supplied value that
   no proof checks. §Soundness describes how that risk is closed structurally.
4. **When a check relates two columns, run it in the part where the smaller column is the
   consumed one.** `verify_input_roots` compares `replay.input_roots` (a few 32-byte roots) with
   the selection witnesses (payload bytes). If it stays in `io`, `io` has to consume the large
   `selection` column. Moved to `store`, it consumes only the small `input_roots` part of `replay`.

The `replay` digest is **structured**: `H(input_commitment ‖ H(output_bytes) ‖ output_root ‖
H(input_roots) ‖ H(draft_transition) ‖ H(recur))`. With this, `store` can consume only
`input_roots` plus the five sibling hashes, and `drafts` only `draft_transition`. No consumer has
to re-read a large `output_bytes`. The structured digest is a host-side definition only. The tile
guest's journal and `env::verify` stay the same, and `io` computes this digest from the journal
bytes it has just verified.

### Part journal

Every part commits the same envelope, which the aggregator can compare without knowing which part
produced it:

```rust
// raster-core/src/parts.rs
pub struct PartJournal {
    pub part: PartKind,
    pub program_commitment: [u8; 32], // sha256(domain ‖ program.bin), as today
    pub window_id: [u8; 32],          // sha256(postcard(InitTransition))
    pub steps: StepChain,             // (count, H(… H(H(seed, item(r0)), item(r1)) …))
    pub produced: Vec<(Column, [u8; 32])>,
    pub consumed: Vec<(Column, [u8; 32])>,
    pub out: PartOutput,              // part-specific, see below
}

pub enum PartOutput {
    Trace { verdict: WindowVerdict, refuted_trace_commitment: [u8; 32],
            window_is_terminal: bool, final_committed_root: Option<[u8; 32]> },
    Entrypoint { entrypoint: EntrypointAuthorization, output: OutputAuthorization },
    Store { root_after: [u8; 32], index_root_after: [u8; 32] },
    ControlFlow, Recur, Io, Drafts,
}
```

`StepChain` uses `hash_trace_item`, the same per-record hash the trace frontier uses, so every part
commits to the same records in the same order. The cost is one SHA over each record plus one chain
hash per step in each part.

### The aggregator (the new `guests/transition`)

```rust
fn main() {
    let p = AggregatorParams::read();         // program frame, InitTransition, auth journal
    assert!(verify_authorization_journal(&p.auth, &p.auth_image_id));   // once, not per step
    let parts: Vec<PartJournal> = read_framed();
    assert_one_per_kind(&parts);                                         // every PartKind, exactly once

    for j in &parts {
        env::verify(PART_IMAGE_IDS[j.part], &to_vec(j)).unwrap();       // pinned, see §Pinning
        assert_eq!(j.program_commitment, p.program_commitment);
        assert_eq!(j.window_id, sha256(&p.init_state));
        assert_eq!(j.steps, parts[0].steps);                             // same records, same order
    }
    for column in Column::ALL {
        let owned = produced_by(&parts, column.owner());                 // exactly one
        for consumer in column.consumers() {
            assert_eq!(consumed_by(&parts, consumer, column), owned);
        }
    }
    assert_eq!(consumed_by(&parts, Entrypoint, Column::AuthJournal), sha256(&p.auth));

    let trace = parts.trace();
    assert!(matches!(trace.verdict, WindowVerdict::DivergesAtLast));

    env::commit(&TransitionJournal {
        init_state: p.init_state,
        current_state: TransitionState::Finished,
        refuted_trace_commitment: trace.refuted_trace_commitment,
        window_is_terminal: trace.window_is_terminal,
        final_committed_root: trace.final_committed_root,
        entrypoint_authorization: parts.entrypoint().entrypoint,
        output_authorization: parts.entrypoint().output,
        /* image ids, manifest commitment, program_commitment as today */
    });
}
```

The aggregator runs no check of its own. Its cost depends on the number of parts, not on the
window length: it reads one journal per part and the column digests, never the columns
themselves.

`TransitionState::Next` and `Transition` drop out of the proving path. Nothing outside the guest
consumes them (`step_transitions` in `src/transition.rs` is the only producer, and
`commands/prove.rs:640` reads only the final receipt).

### What one part looks like

Each part's `main.rs` is a loop of about 40 lines around the moved check functions. `store`:

```rust
fn main() {
    let p: PartParams = read_framed();                    // program frame, InitTransition, n_steps
    let mut steps = StepChain::new();
    let mut frontier = deserialize_frontier(&p.init.init_storage_frontier).unwrap();
    let mut index_root = p.init.init_storage_index_root.clone();
    let mut roots = ColumnWriter::new(Column::StorageRootsBefore);
    let mut selection = ColumnWriter::new(Column::Selection);
    let mut replay = ColumnReader::new(Column::Replay);   // consumes input_roots + siblings

    for _ in 0..p.n_steps {
        let s: StoreStep = read_framed();                 // record + only storage witnesses
        steps.push(&s.record);
        roots.push(&(frontier_root(&frontier), index_root.clone()));
        let (_, _, next) = checks::store::verify_storage_transition(
            &s.record, s.input_source_witness.as_ref(), &s.selections,
            s.output_witness.as_ref(), s.storage_witness.as_ref(),
            &mut frontier, &index_root);
        if let Some(view) = replay.next(&s.replay_view) {          // folds into the digest
            checks::store::verify_input_roots(&s.record, view.input_roots(),
                                              s.input_source_witness.as_ref(), &s.selections);
        }
        selection.push(&s.selections);
        index_root = next;
    }
    commit_part(PartKind::Store, &p, steps,
                [roots.finish(), selection.finish()], [replay.finish()],
                PartOutput::Store { root_after: frontier_root(&frontier), index_root_after: index_root });
}
```

## Repository layout

```
crates/raster-core/src/
├── transition.rs                 # unchanged: InitTransition, TransitionJournal (Finished)
└── parts.rs                      # NEW: PartKind, PartJournal, PartOutput, Column (+ owner and
                                  #      consumers tables), StepChain, ColumnWriter/Reader,
                                  #      ReplayDigest, per-part step input types

crates/raster-prover/
├── build.rs                      # embeds part guests, then the aggregator (§Pinning)
├── checks/                       # NEW lib crate `transition-checks`, no `main`
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── merkle_tree.rs        # moved from guests/transition
│       ├── window.rs             # assert_window_is_commitment_slice, assert_opens_at_genesis,
│       │                         #   finalize's compare rule (from fraud_proof.rs)
│       ├── cfs.rs                # moved, minus the recur half
│       ├── recur.rs              # advance_recur_progress + helpers, seed diagnostic
│       ├── io.rs
│       ├── store.rs              # + verify_input_roots
│       ├── drafts.rs
│       ├── entrypoint.rs
│       └── tests/                # tests.rs split by the file under test
│           ├── cfs.rs  recur.rs  io.rs  store.rs  drafts.rs  entrypoint.rs  window.rs
│           └── columns.rs        # every column, tampered by one byte → its consumer's digest moves
├── guests/
│   ├── authorization/            # unchanged
│   ├── chain_fraud/              # unchanged source; pins TRANSITION_GUEST_ID (the aggregator)
│   ├── part_trace/src/main.rs
│   ├── part_control_flow/src/main.rs
│   ├── part_recur/src/main.rs
│   ├── part_io/src/main.rs
│   ├── part_store/src/main.rs
│   ├── part_drafts/src/main.rs
│   ├── part_entrypoint/src/main.rs
│   └── transition/src/main.rs    # the aggregator: keeps the name, so TRANSITION_GUEST_ID keeps
│                                 #   meaning "the guest whose Finished journal is a fraud proof"
└── src/
    ├── transition.rs             # step_transitions → prove_window (same arguments)
    └── parts/
        ├── mod.rs                # prove_window: build table → prove parts (parallel) → aggregate
        ├── table.rs              # BoundaryTable: every column, computed host-side from the window
        └── inputs.rs             # per-part step inputs (build_transition_input, split per part)
```

The host already has every column before any proving starts: it has the trace, the recorder's
storage, and the replay results. Today it gets them by running the steps one after another.
`BoundaryTable` computes them in a single pass, so all parts can start at the same time.

## Worked example: a three-step window

The window ends a recur-tile loop and the program. Step `i` is the loop's last tile iteration and
writes a draft. Step `i+1` is the `SequenceEnd` that returns the site's state. Step `i+2` is
`ProgramEnd`, where the divergence is claimed.

| part | step `i`: tile iteration | step `i+1`: `SequenceEnd` with state | step `i+2`: `ProgramEnd` |
| --- | --- | --- | --- |
| `trace` | check exec index, append, compare: must **match** | append, compare: must **match** | append, compare: must **diverge** → `DivergesAtLast` |
| `control_flow` | sequence id; step inputs follow the CFS bindings, using `replay`; expected coordinates | sequence id; scope parent is in the trace, using `trace_root_before` | must be expected; nothing may follow it |
| `recur` | iteration item against `selection`; draft continues the site root, using `draft_step`; recorded state = replay state | returned state read against `storage_roots_before`; pop frame | stack must be empty |
| `io` | `env::verify` replay against the registry id; output bytes = witness → produces `replay` | I/O bytes against record commitments | — |
| `store` | roots before = frontier; no write (D3) → roots unchanged; `input_roots` = selected roots | no roots, skip | no write |
| `drafts` | pre-state against `replay.draft_transition.root_before`; re-apply the ops → produces `draft_step` | — | — |
| `entrypoint` | — | — | output read against `storage_roots_before`; `Pending` → `Established` |

Aggregator: every part's receipt verified, the same `steps` in all of them, every column closed, verdict
`DivergesAtLast` → commits `Finished`.

## Soundness

**Claim:** the aggregator accepts exactly when the monolithic guest would have reached `Finished`
on the same window.

The monolith is the conjunction of the per-step checks over one shared state. Each part is the
conjunction of its own checks over its own state, with the other parts' state replaced by host
values. The aggregator restores the conjunction:
- every part processed the same records in the same order (`steps`),
- from the same initial state (`window_id`) and program (`program_commitment`),
- and every host value a part consumed equals the value the owning part computed (column
  digests, which depend on collision resistance of SHA-256).

Under those three conditions every consumed value is the value the monolith would have had in that
local variable, so each check sees the same inputs as in the monolith.

**The hazard is a missing column.** If some cross-check read is not mapped to a column, the part
reads a host value that no proof checks. Three structural defenses:
1. **Part input types contain no free state.** The `*Step` structs in `parts.rs` can hold a root, a
   journal or a `DraftStep` only as `Consumed<Column, T>`, and only `ColumnReader` can unwrap one.
   Unwrapping folds the value into the digest. Code review then only has to check the types, not
   every call.
2. **`Column::consumers()` is the single source of truth.** Parts use it to decide what they
   commit, and the aggregator uses it to decide what it equates. A part that consumes a column the
   table does not list fails `commit_part` with an assert.
3. **Tamper tests per column** (`checks/tests/columns.rs`) and a **differential test**: the
   monolith and the split pipeline must accept and reject the same set of fixtures, including the
   existing fraud fixtures and every negative test in today's `tests.rs`.

**Ordering inside a step is preserved, because the ordering that matters is within a part.** For
example, rule 8 runs before `advance_tile_iteration` in `recur`, and `verify_bound_index_bindings`
runs after the per-binding loop in `store`. The ordering *between* parts is fixed by how the columns
are defined: `storage_roots_before` and `trace_root_before` are the pre-step values, which is what
the monolith passes today (the comments at the `advance_recur_progress` and `verify_program_end`
call sites already state this).

## Cost

Nothing here has been measured. What changes:

| goes away | is added |
| --- | --- |
| N−1 `env::verify` of a previous `TransitionJournal` | one `env::verify` per part journal, in the aggregator |
| N decodes of `program.bin` + `CfsCursor::new` | one per part |
| N `env::verify` of the authorization journal | 1 |
| N receipts, each lifted and resolved | one receipt per part, plus the aggregator's |
| every step deserializes all witnesses | each part deserializes only its own; the record and anchored witnesses are read once by every part that needs them |
| proving that is sequential across steps | parts are independent: wall clock is the largest part + the aggregator, given enough provers |
| | per part and step: `hash_trace_item` + chain hash + column digest updates |

On **one GPU**, parallelism does not help, and the gain is only the left column minus the right
column. That gain grows with N, but how much depends on how the ~2.4M user cycles per step divide
between the checks and the fixed overhead, and that split is not known yet. **Step 0 below has to
come first:** if `io` or `store` turns out to be 80% of a step, the critical path barely gets
shorter, and the case for the split rests on points 3–5 of §Problem rather than on speed.

## Image-id pinning

The aggregator must accept only the real part guests. Two options:

- **(a) Compile them in (recommended).** Embed the parts in a first `risc0_build` pass, write their
  ids to a generated `part_ids.rs`, and build the aggregator in a second pass that `include!`s that
  file. The aggregator's image id then commits to the full set of parts, and `chain_fraud` and
  relying parties go on pinning a single id. Open question: whether `risc0_build` 3.0 can pass the
  generated path to the guest build. This needs a prototype before implementation.
- **(b) Read them as input.** The aggregator takes a `PartSet`, hashes it into a new
  `TransitionJournal::part_set` field, and relying parties pin that value. This is the pattern
  `chain_fraud` already uses for its inner ids. It works without build changes, but it changes the
  journal and adds a second value every verifier has to pin.

Either way, changing any check changes the aggregator's identity, as it does today. The split does
not make check upgrades cheaper to deploy.

## Migration

0. **Measure.** Wrap each call in `apply_verified_step` with `env::cycle_count()` and run the
   `prompt-prepare` 16-step benchmark. Record the per-check split and the fixed per-step overhead.
   This decides whether to go ahead, and whether `recur` stays separate.
1. **Extract `transition-checks`.** Move `checks/*`, `merkle_tree.rs` and the window helpers into
   the library crate, and make `guests/transition` depend on it. There is no behaviour change, but
   the image id changes, as any dependency change does.
2. **Make the columns explicit inside the monolith.** Change the signatures so that every
   cross-check value is passed as `Consumed<…>`, still inside one guest. This is where a missed
   dependency shows up, as a compile error, before any part exists. Commit this step on its own;
   it is worth having even if the split stops here.
3. **Add the parts and the aggregator** behind `cargo raster prove --transition=split`, with the
   monolith as the default. Add `BoundaryTable` and `prove_window`.
4. **Run the differential test** across every fraud fixture and negative test, and the step-0
   benchmark against both pipelines.
5. **Switch the default, then delete the monolith**, along with `TransitionState::Next`,
   `Transition`, and the per-step recursion helpers in `fraud_proof.rs`.

## Open questions

1. **Pinning:** option (a) or (b), see above.
2. **Window length per part.** Each part is one execution over the whole window. risc0's
   continuations split it into segments, but host executor memory grows with N. If windows grow
   well beyond 32, a part can be proven in chunks that resume from the previous chunk's journal,
   which is today's `Next` mechanism applied per part. This is deferred until needed.
3. **Should `recur` stay a separate part?** Answered by step 0.
4. **Duplicate reads of anchored witnesses.** If step 0 shows that re-hashing large I/O bytes in
   several parts is significant, promote those witnesses to columns as well. That trades hashes for
   digest equalities.
