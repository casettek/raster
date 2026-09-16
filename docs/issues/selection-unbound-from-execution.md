# Issue: `selection-unbound-from-execution` — a verified selection proof is never joined to the execution it authorizes

Status: open 2026-09-16. Unowned. **Top priority** — set 2026-09-16.

Reproducible against `feature/recur-mid-seed` at `0a71d10`. Every citation is committed code.

§2a's half is **demonstrated**, not only reasoned — see §3. The fabricated sweep is built and
run against the real rule machinery, and the rules close it clean.

This is the only open issue in this directory that is a **soundness** gap rather than a
fail-closed defect. `authenticated-chain-draft-output` panics, `chain-shape-count-unverified`
needs a tampered sidecar and moves no commitment, `fraud-window-trace-ends` degenerates to a
window that fails closed. This one makes the transition guest — which *is* the definition of a
valid execution, since anything it accepts is accepted — hold a fabricated recur sweep to be
valid.

Related:

- [`lazy-list-recur.md`](../proposals/lazy-list-recur.md) — **names both halves and owns
  neither.** §Outstanding at implementation (l.1024) records *"Rule 8 is not implemented"* as the
  one missing *check* against its own §Verification, and the last row of its claim table (l.793)
  records *"each tile consumed the value at the index it claims"* as **not proved**, adding that
  the binding *"was never in scope here and is owned by no proposal."* That proposal's phases 1–6
  are implemented; nothing is going to pick these up as a side effect of finishing it.
- [`paged-bytes.md`](../proposals/paged-bytes.md) §3.3 (l.547) — states the second half generically
  (*"a pre-existing gap affecting every Rastered type"*) and makes it a **release gate**: *"`Bytes`
  must not be described as end-to-end authorization-sound until it lands."* Its status line (l.3)
  carries both as Gate 2 and Gate 3, open. It sketches a fix — tile guests commit a structural
  root per decoded input — but it is a gate on `Bytes`, not a design anyone owns, and the proposal
  shipped without it.
- [`dynamic-index-selection.md`](../proposals/dynamic-index-selection.md) (phases 1–3 implemented)
  — supplies `verify_bound_index_bindings`, which pins where a dynamic *index* came from. Adjacent
  and insufficient: it binds one binding's index to another binding's authorized value, never a
  binding's payload to the bytes a tile ran on.
- [`recur-progress-commitment.md`](../proposals/recur-progress-commitment.md) (rev 2 implemented)
  — owns the frame machinery the rules run on. Its own framing is the reason this matters: the
  journal is *"a binding, not an authority."*

## 1. The joint that is missing

Two objects describe one recur iteration, and no assertion connects them.

**The selection proof** says: *these bytes are the contiguous slice `[start, start + k)` of the
list committed at the source's storage coordinates.* It is verified on every execution step,
including recur iterations, by `verify_storage_transition`
(`crates/raster-prover/guests/transition/src/checks/store.rs:190-205`) — coordinate-index and
append-log membership, `storage_meta.commitment == storage_meta.selection.source_root_hash`
(`:186`), then `verify_selection_witness` (`crates/raster-core/src/input.rs:1321`) folding the
payload up to that root. The `start` is pinned to the selector's `Range` segment
(`step_proves_segment`, `input.rs:890-893`) and `fold_list_range` rejects `start + k > len`.

**The replay journal** says: *iteration `i` of `⌈L/C⌉` consumed `n` elements and returned
`Continue`/`Break`* (`RecurPosition`, `crates/raster-core/src/draft.rs:105-118`), proven by
`env::verify` against the registry image id, and bound to the recorded tile input by
`replay_journal.input_commitment == sha256(input_witness_bytes)`
(`checks/io.rs:109-113`).

The completeness rules run on the second object only. `authenticated_source_len`
(`checks/cfs.rs:500`) reads `L` from the site's authenticated `0x0A` list metadata at
`SequenceStart`; `advance_tile_iteration` (`crates/raster-core/src/recur_progress.rs:371`) then
applies rule 3 (`declared_iterations == ⌈L/C⌉`, `:401`) and rule 4
(`consumed_elements == min(C, L − consumed_total)`, `:411-434`) to the journal's numbers, and
`close_site` (`:517`) applies rules 5–7.

So `start` — the one recorded fact that says *where in the source this iteration read* — is
proved against the Merkle root and then **compared to nothing**. And `consumed_elements` — the
one fact the coverage argument is built from — is compared only against other numbers the same
journal supplies.

## 2. The two halves

### 2a. Rule 8: the range selection is never cross-checked against the journal

`lazy-list-recur.md` §6 (l.725-742) specifies the cross-check and tabulates why: the proof and
the journal carry the same three facts by independent routes, and requiring them to agree is what
makes coverage rest on something a prover cannot choose freely.

| fact | journal | selection proof |
| --- | --- | --- |
| source length | `L` from §1 metadata | `ListRange.len` |
| where the iteration sat | running total of `consumed_elements` | `ListRange.start` |
| how much it took | `consumed_elements` | payload element count `k` |

None of it is implemented. `grep -rn ListRange crates/raster-prover/guests/` returns **zero**
hits. Nothing anywhere asserts `ListRange.len == L`, `ListRange.start == covered_before`, or
`k == consumed_elements`.

The host does compute the honest range — `recur_chunk_input` (`crates/raster/src/input.rs:2517`)
takes `start = index * chunk` and `end = min(start + chunk, len)` and `select_range` (`:1604`)
pushes it onto the selector (`:1648`) — so the correct value is present in every honest trace.
It is simply never required.

### 2b. §3.3: the proven bytes are never the bytes the tile ran on

A step carries two independent byte strings:

- `input_witness_bytes`, hashed into `step_record.input_commitment` and equated with the replay
  journal's `input_commitment` (`checks/io.rs:105-113`) — *the tile ran on these*;
- `storage_selection_witnesses[binding].bytes`, folded to the source root
  (`checks/store.rs:190-205`) — *these were selected from the committed artifact*.

No check relates them. `paged-bytes.md:551-553` states the reason plainly: *"Nothing asserts they
are the same value, because the guest never decodes the postcard input."* The audit proves some
bytes were selected and that some bytes were executed, in the same step, about the same binding —
and stops.

## 3. Why these are one issue and not two

Neither half closes the hole alone, and both close the same joint, so splitting them would
produce two proposals each able to claim the other is load-bearing.

- **Land 2a only.** The range is pinned to the sweep position, but the tile's input is still
  arbitrary bytes. A prover selects the honest chunk `i`, proves it, and replays the tile on
  something else. Rules 1–8 all pass.
- **Land 2b only.** The tile provably ran on the selected slice, but the slice is unanchored. A
  prover selects `Range { start: 0, end: 2 }` on *every* iteration of a `chunk = 2, L = 10` sweep.
  Each selection proof verifies. Each journal reports `consumed_elements = 2`, which rule 4
  accepts because `min(2, 10 − 2i) = 2` for `i < 5`; rule 3 sees `declared_iterations = 5 = ⌈10/2⌉`;
  rules 1–2 see indices `0..5`; rule 5 sees a complete prefix; the site closes clean. The trace
  claims to have swept a 10-element list and read the first two elements five times.

  **Demonstrated 2026-09-16**, driving the real machinery rather than by inspection:
  `poc_a_sweep_that_rereads_the_first_chunk_passes_every_completeness_rule`
  (`crates/raster-core/src/recur_progress.rs`) builds exactly this `L = 10, chunk = 2` case and
  closes the site clean. It then states the hole as an equality rather than a story: the honest
  ranges `[0,2) [2,4) [4,6) [6,8) [8,10)` and the fabricated `[0,2)×5` genuinely differ, yet the
  tuples handed to `advance_tile_iteration` are **identical**, so no completeness rule can
  separate them. A companion test shows chunking is not the cause — `consumed_elements` carries no
  position at any chunk size, so `chunk = 1` is exposed the same way.

  The other half needs no construction. `verify_selection_witness(commitment, witness)`
  (`raster-core/src/input.rs:1321`) is pure in exactly two arguments, neither of which carries an
  iteration index or a position, so the same valid `Range { start: 0, end: 2 }` witness verifies
  on every call. There is no cross-step state that could notice — consistent with the zero
  `ListRange` hits in the guest.

  Not demonstrated: the converse, **land 2a only**. That one rests on a tile being replayable on
  arbitrary bytes, which is `checks/io.rs:105-113` binding the journal to the *recorded witness*
  rather than to the selection — read, but not exercised.

## 4. What it costs today

`examples/hello-tiles/src/main.rs:122-125` is a live `chunk = 2` sweep, so this is not
hypothetical scaffolding.

What the guest currently establishes for a chunked recur site, stated exactly:

| claim | status |
| --- | --- |
| the source list has exactly `L` elements | proved (authenticated metadata, `checks/cfs.rs:500`) |
| the sweep ran `⌈L/C⌉` iterations, indexed contiguously from 0 | proved (rules 1–3) |
| each iteration's *claimed* consumption has the declared chunk shape | proved (rule 4) |
| the sweep ended complete, or at a replay-proven `Break` | proved (rules 5–7) |
| each iteration's selection is a real slice of the committed source | proved (`checks/store.rs`) |
| **the sweep's selections tile `[0, L)` without gaps or repeats** | **not proved** (2a) |
| **each tile consumed the value its selection proves** | **not proved** (2b) |

The bottom two are the ones a reader assumes from the top five. Any soundness claim, release note
or `SKILL.md` line about "authenticated iteration" should quote this table rather than the phrase.

A secondary consequence, worth naming because it will otherwise be rediscovered: the prover's
`resolve_inputs_sources` returns no source records for an iteration
(`crates/raster-prover/src/trace.rs:864`), mirroring the guest's early return
(`checks/cfs.rs:212`). That mirroring is correct — an iteration binds no CFS inputs — and it
is *also* why neither side has an obvious place to hang the missing checks. Whatever closes this
has to decide where an iteration's per-step obligations live, given that the CFS-binding path
deliberately skips them.

⚠️ **Amended 2026-09-16.** Less true than when written. `trace-leaf-field-binding` added
`verify_exec_index` and `verify_sequence_id` in `LiveTransition::apply_verified_step` — *outside*
`verify_step_record_inputs`, so they run for **every** step including recur iterations, which is
precisely the set the CFS-binding path skips. A recur iteration therefore now does carry per-step
obligations, and §5's *per-step obligation record* direction has a structural hook it did not
have. The line numbers above also moved with that work; they are updated.

## 5. Directions

Shapes only; none is picked.

- **Cross-check inside `advance_recur_progress`.** The iteration branch (`checks/cfs.rs:612-655`)
  already holds the journal, and `storage_selection_witnesses` is already threaded into the same
  function for `authenticated_source_len`. Rule 8 could be a few asserts there. Cheapest for 2a;
  does nothing for 2b; leaves the two halves in different modules.
- **A per-step obligation record.** Give a recur iteration an explicit "what this step must prove"
  structure, resolved where `resolve_inputs_sources` bails today, so the prover and the guest
  share one list rather than two matched early returns. More surface; addresses §4's last
  paragraph directly.
- **Structural roots in the replay journal** (`paged-bytes.md` §3.3's sketch). Tile guests commit
  the structural root of each decoded input; the audit requires each storage-bound input root to
  equal its verified selection root. This is the only sketch on the table for 2b, it is generic
  across every Rastered type rather than recur-specific, and it moves every tile image id — so
  every `program_commitment` — which is the cost to argue about.
- **Widen the journal's position facts** so `RecurPosition` carries the range it read, not only
  how much. Makes 2a an equality on one object instead of a join across two; costs a trace-format
  break and leaves the journal still self-reported, so it does not substitute for the fold.

## 6. What this is not

- Not `S2` (`lazy-list-recur.md:1039`): `advance_sequence_iteration` firing only on
  `SequenceStart`, so an iteration's `End` is never paired with its `Start`. Same proposal's
  outstanding list, different joint, cheap and separable — kept out so it cannot be used to
  enlarge whatever closes this.
- Not the unrun peak-RSS benchmark or the missing `select_range` proof matrix
  (`lazy-list-recur.md:1044-1063`). Those are missing *evidence*; this is a missing *check*.
- Not `window-seed-reconstruction`'s mid-loop window gap, which is implemented as of 2026-09-09.
- Not about unauthenticated runs: `--no-auth` emits no trace, so there is nothing to defend.
