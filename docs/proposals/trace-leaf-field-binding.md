# Proposal: `trace-leaf-field-binding` — verify everything the trace leaf commits to

Status: **implemented** (2026-09-16)

Found while planning [`fraud-window-trace-ends`](../issues/fraud-window-trace-ends.md), and
larger than the issue that led to it: three of the four defects here are soundness breaks that
let a dishonest challenger forge a fraud receipt against an **honest** prover. None of them has
anything to do with the ends of a trace.

The window-geometry half of that issue is owned by
[`trace-end-windows`](./trace-end-windows.md), which **depends on this one**: its argument that a
one-item genesis window is provable requires `ProgramStart` to be uniquely determined, and it is
not while any hashed field goes unverified.

Related:

- [`chain-fraud-proof.md`](./chain-fraud-proof.md) — `assert_window_is_commitment_slice` closed its
  problem 3 (a challenger proving divergence against a fabricated fingerprint). It binds the
  window's *content* to the commitment. It never bound the window's *shape*, and nothing bound
  the step records' unverified fields.
- [`chain-io-commitment.md`](./chain-io-commitment.md) §3 — the dispute path these receipts feed.
  A forged receipt condemns an honest stage.

## The rule

> **A field that reaches the trace leaf must be checked against something authorized, or derived
> from position. Anything else is free entropy an attacker can use to manufacture a divergence.**

The leaf is `sha256(postcard(StepRecord))` over the **whole** record, so every field reaches it,
and through it the trace root and the fingerprint entry. The attack is the same in each case:

```
take an honest window   s_0 .. s_{L-1}
submit                  s_0 .. s_{L-2}   unchanged   -> match the commitment, margin satisfied
                        s_{L-1}' = s_{L-1} with one unverified field changed
                                                     -> different leaf, root, fingerprint entry
                                                     -> assert!(diverges) passes -> Finished
```

The window's margin pins the *opening state*. It has never pinned the *diverging item*, and that
item only needs one field nobody checks.

## What was unverified

### 1. `exec_index` — nothing read it at all

```
$ grep -rn "exec_index" crates/raster-prover/guests/transition/src/ --exclude=tests.rs
(no matches)
```

`StepRecord`'s first field. Demonstrated before fixing: a record and its `exec_index`-bumped twin
passed identical verification and hashed to different leaves and roots.

**Fixed by derivation.** `TraceRecorder::new` starts the counter at 0 and `record` increments
before use, and the single production call site pushes every returned record unconditionally
(`raster-cli::commands::run::record_trace_event`) — so the step at trace index `t` carries
`exec_index == t + 1`. The guest already holds `t`: the frontier's position *before* the step is
appended is the step's trace index. `verify_exec_index` is one comparison and needs no new
carried state.

That invariant was confirmed before relying on it, not assumed. An earlier plan preferred
window-local continuity (`+1` between consecutive items) on the grounds that the global numbering
was unverified; checking showed both need the same "no dropped records" property, so the weaker
rule bought nothing and could not cover a one-item window.

### 2. `sequence_id` — checked for two step kinds out of five

`record_matches_item` compared it only for `SequenceStart`/`SequenceEnd`. The `Exec` arms compare
the *target name* instead, and the program boundaries never reached the check at all —
`verify_step_record_inputs` returns early on empty coordinates, and on recur-iteration
coordinates.

The field carries two different things, which is why `verify_sequence_id` is not one lookup:

| step kind | `sequence_id` holds |
|---|---|
| `SequenceStart` / `SequenceEnd` | the **callee** — the sequence entered or left |
| `Exec`, `ProgramStart`, `ProgramEnd` | the **frame** it executes in |

And the two recur kinds part company, which was the branch worth confirming rather than inferring:

| shape | frame |
|---|---|
| recur **tile** — iterations at `site ++ [i]`, closing `Exec` at `site` | pushes **no** frame → both name the *containing* sequence |
| recur **sequence** — body at `site ++ [i] ++ [k]` | pushes a frame → body names the **site** |

Confirmed by writing the hypothesis as a test in the recorder
(`sequence_id_names_the_callee_at_boundaries_and_the_frame_everywhere_else`) before implementing
the derivation. Nothing had tested that vocabulary; now the producer and the verifier are pinned
to the same table from both sides, so drift fails one of the two.

### 3. The sequence-scope witness was bound to nothing

`SequenceScope { i }` claims "my input is parameter `i` of the frame I am in". The guest checked
it by comparing the step's own resolved source against argument `i` of the **parent's**
`FnInput` — supplied by the host and tied to nothing. So `assert_same_source` compared a value
against a value the same party chose, and passed for any claim at all. Demonstrated by inventing
two mutually exclusive parents, each accepted because each was built to agree with the step.

Worst on the *positive* path: a terminal-window receipt could attest to an execution whose step
read a value it was never given.

**Fixed with machinery that already existed and was discarded.**
`TransitionInput::input_sources_witnesses` carries Merkle paths proving each source record is in
the trace. The host has always shipped it and **no guest code read it** — defect 4, closed by
fixing defect 3. `verify_sequence_scope_parent` now pins the parent three ways: it is in the
trace (folded against the same pre-append frontier root `verify_exec_index` uses), it is the
`SequenceStart` at this step's parent frame coordinates, and its recorded
`input_source_commitment` is the commitment of the supplied `FnInput`.

Coordinates carry the iteration index inside recur sites, so they name one invocation rather than
a set — which is what makes the second condition sufficient.

### 4. The window's shape was unbound

The guest never learned `window_size` — it is `revealed_items.len()`, absent from the header, and
`revealed_items_commitment` is never opened. So `window_len` (from the challenger's
`Fingerprint::len`, a metadata field stored verbatim without checking it against `bits`) and
`window_start` (from the challenger's frontier position) were constrained only by
`window_start + window_len <= fingerprint_len`.

A two-item window is the degenerate case rather than a merely unusual one. Over `L` items
`finalize` never compared item 0, required items `1..L-2` to match, and required item `L-1` to
diverge — so at `L = 2` there were **zero** matching comparisons and nothing pinned the
challenger's opening state. The existing test suite already documented the hole without naming
it: `accepts_a_window_slice_crossing_a_block_boundary` asserts a 4-item window at offset 14 of a
40-item commitment is accepted.

**Fixed** by putting `window_size` in the header and asserting `window_len == header.window_size`.
`trace-end-windows` relaxes that at genesis, which is why the two proposals land together.

## A prerequisite that was its own defect

`resolve_inputs_sources` scanned for the `SequenceStart` that opened a step's parent frame. For a
top-level step at `[k]` that frame is `[]`, which `ProgramStart` opens — so **every top-level
step with a non-inline input panicked**, from both `verify` and `terminal_window`. The witness map
defect 3 needs could not be built at all.

It survived because the unit fixtures opened `main` with a `SequenceStart` at `[]`, a shape the
recorder never produces. Five fixtures modelled a dead trace format, so the suite validated a path
production never takes. Fixed by teaching the lookup that `ProgramStart` opens the root frame, and
by migrating every fixture.

Two further fixture defects of the same class surfaced and were fixed: a CFS declaring a
`SequenceScope` binding at the **root** frame (which the compiler cannot emit — `main` has no
caller, so its parameters resolve to `EntryArgument`), and, following from it, **no test anywhere
exercising `SequenceScope` resolution at a nested frame** — the arm defect 3 is about.

## Soundness

Every field reaching the leaf is now accounted for:

| field | pinned by |
|---|---|
| `exec_index` | derived from the frontier's position |
| `sequence_id` | derived from the CFS and the step's coordinates |
| `coordinates` | `get_next_expected_coordinates`, `entrypoint::verify_step` |
| `kind` | the per-kind checks |
| `recur_progress_commitment` | `advance_recur_progress` |
| `recur_state` | `checks::cfs` |

## Modules touched

- `guests/transition/src/checks/cfs.rs` — `verify_exec_index`, `verify_sequence_id`,
  `verify_sequence_scope_parent`, `assert_record_in_trace`
- `guests/transition/src/fraud_proof.rs` — the three calls, the shape assert
- `raster-core/src/transition.rs` — `window_size` on the header
- `raster-prover/src/trace.rs` — `frame_opening_index`, `program_start_index`, fixture migration
- `raster-runtime/src/tracing/recorder.rs` — the `sequence_id` vocabulary test

## Verification

Each defect was **demonstrated before being fixed**, and each proof of concept was then inverted
into a regression test rather than deleted. That order was deliberate: it settles that the hole is
real before soundness-critical code moves, and it produces a test that fails for the right reason.

The one exception is the host-side `exec_index` test, which is *kept* rather than inverted: the
host should still detect the tampering, because the traces genuinely differ. What changed is that
the guest refuses to prove it. It now reads
`exec_index_tampering_is_detected_but_no_longer_provable` and points at the guest test that
refuses.

## Outstanding

- **`input_sources_witnesses` is still cloned whole into every step**
  (`raster-prover/src/transition.rs`), so a window of `w` steps carries roughly `w²` entries. Now
  that the guest reads the map this is worth narrowing, but narrowing it wrongly drops a witness
  an honest proof needs — a liveness break, worse than the size. Measure first; keep the filter a
  superset.
- `verify_sequence_scope_parent` identifies the parent by coordinates, which name one invocation
  because recur coordinates carry the iteration index. That reasoning should be checked against
  any future construct that reuses coordinates across invocations.
- The `Next` path of the transition chain has no end-to-end coverage (pre-existing).
