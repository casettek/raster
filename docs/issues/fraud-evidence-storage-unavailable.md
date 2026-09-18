- ~~**A recur site's source is recorded as a raw payload, not list metadata**~~ — **fixed
  2026-09-18.** `lazy-list-recur` §2 reached the recur *tile* macro and never the recur *sequence*
  one, though §3 says it binds both: `call_recur!` traced its source with `recur_source_trace`
  (`recur.rs:745`, `:870`) while `call_recur_seq!` used `auth_ref_trace` (`:1376`) — carrying,
  verbatim, the comment describing the `0x0A` metadata selection it did not make. Two costs in one
  line: `authenticated_source_len` refuses anything but `List`, so no fraud proof covering a recur
  sequence site could be built; and `auth_ref_trace` resolves the binding, materializing the whole
  list before any runner runs — §2's *"earliest and largest of the three eager paths"*, exactly what
  that proposal exists to remove. Pinned by
  `a_recur_sequence_site_commits_to_its_source_list_metadata`, which fails with the production
  symptom when the one-line fix is reverted.

- ~~**A window step is not where the CFS expects it**~~ — **fixed 2026-09-18.** `checks/cfs.rs` →
  `Step [2, 0, 2] is not among [[2, 0, 1], [2, 0, 1, 0], [2, 0]]` on the honest transition from
  iteration 1's end to iteration 2's start. `try_get_next_coordinates` took only a position, and a
  recur sequence iteration's `Start` and `End` shared one — so it answered a close with the
  *open's* successors, which exclude the next iteration.

  Closed by giving a close its own coordinate. `CfsCoordinate` is now `i32` and positions are
  **1-based** (`FIRST_COORDINATE`), which makes the bracket symmetric: open `i` closes at `-i`,
  with no offset to carry. At 0-based it could not be — `-0 == 0` would fold the first iteration's
  close back onto its open — and 1-basing also makes `0` an invalid position, so a stray `vec![0]`
  fails to resolve instead of silently naming the first item. `DRAFT_NAMESPACE` moved from
  `u32::MAX` to `i32::MIN`.

  Two conventions meet in exactly two places, both commented: the progress rules count iterations
  from 0 (the same 0 a recur tile's replay journal reports), and `intra_sequence_item_index` is a
  0-based index into `items` rather than a coordinate. Everything else resolves through
  `CfsCoordinates::opened()`, which is why the guest needed no changes at all.

  **Cost:** every coordinate in every trace changes, so commitments move again.

- ~~**Replay journal decode failures**~~ — **fixed 2026-09-18 by rebuilding the tile guests.**
  `Failed to decode replay journal: Hit the end of buffer` printed on every run of this
  investigation (7 in the first, 11 by the end) and was non-fatal — until a window landed on an
  affected step, when the replayed result was simply absent
  (`transition.rs:86`, `Replayed result not found for transition step`). After a rebuild: **0
  failures**, in both `prompt-prepare` and `examples/hello-tiles`.

  **The trap, and it is worth its own fix.** `cargo raster build --backend risc0` did **not**
  rebuild them. Tile ELFs are content-hash cached under `target/raster/tiles/<tile>/risc0/`, and
  the key does not include `raster-core` — so guests built 2026-08-14 survived every subsequent
  change to the trace types and were silently reused, emitting journals in a layout the host no
  longer decodes. The only way to force a rebuild was to move the cache directory aside. Nothing
  warns; the symptom surfaces much later as a decode error naming neither the tile nor its age.

  **Verified end to end after the rebuild** (both programs, honest commit `0`, fraud injected,
  audit exits `101` = detected):

  | program | replay failures | transition steps proven |
  | --- | --- | --- |
  | `prompt-prepare` | 0 (was 7–11) | 2 |
  | `examples/hello-tiles` | 0 | 10 |

  `hello-tiles`'s own long-standing blocker — `Missing storage object at
  CfsCoordinates([4294967295, 1])` — is gone too.

- **A sequence-scope parent's trace-inclusion proof does not fold to the trace root** — the current
  blocker, and now reproducible in **two independent programs**, which it was not before:
  `checks/cfs.rs` → `Sequence-scope parent record is not in the trace at the claimed position`,
  from `assert_record_in_trace` in `verify_sequence_scope_parent`. `prompt-prepare` reaches it
  after 2 steps and `hello-tiles` after 10. The host builds that witness in
  `witness_record_inputs` against the trace *prefix* (`trace[..step_index]`); whether the guest's
  `trace_root` at that step is the same prefix is the thing to establish first.

- **Why a `SequenceStart` needs no storage evidence at all** (recorded because an earlier draft of
  this file claimed the opposite). A step with no `StorageRoots` never proposes a root: the guest
  returns its own carried frontier unchanged, and every step that *does* declare roots must match
  it (`Execution-step storage root before does not match current storage root`). So a sequence step
  is transparent to the storage chain — it cannot alter the root because it is never asked for one,
  and giving it roots would be *worse*, handing a prover somewhere to propose one.

  Its arguments are authenticated in three places, none of them a storage witness on the sequence
  step: `input_source_commitment` binds the argument list to the record and the record is
  fingerprint-bound to `commit.bin`; `verify_step_record_inputs` holds those arguments to the CFS;
  and the **consuming** step — which has roots — proves the coordinates are in the store and the
  selection resolves. `checks/cfs.rs`'s `verify_sequence_scope_parent` plus `assert_record_in_trace`
  pin "argument `i` of the parent" to the parent's own record. The obligation is discharged where
  consumption happens, which is the only place there is an authenticated root to check against.

  Open nuance, untraced: a recur sequence *iteration*'s `SequenceStart` returns early from
  `verify_step_record_inputs` into `verify_recur_iteration_chunking`, so the generic CFS binding
  check is skipped there in favour of the recur rules. Whether those cover the same ground is worth
  its own look.

- ~~**A recur site's source is recorded as a raw payload, not list metadata**~~ — **fixed
  2026-09-18.** `checks/cfs.rs` → `Recur site start … must commit to list metadata, not a raw
  payload` (`left: Raw, right: List`) at the `call_recur_seq!` site `[3, 0]`.

  `lazy-list-recur` §2 was implemented for the recur **tile** macro and never for the recur
  **sequence** one, though §3 says it binds both. `call_recur!` traced its source through
  `recur_source_trace` (`recur.rs:745`, `:870`); `call_recur_seq!` used `auth_ref_trace`
  (`:1376`) — while carrying, verbatim, the comment describing the `0x0A` metadata selection it
  did not make.

  **Two costs, one line.** `authenticated_source_len` refuses anything but `List`, so no fraud
  proof covering a recur *sequence* site could be produced at all. And `auth_ref_trace` resolves
  the binding, materializing the whole list before any runner runs — §2's *"earliest and largest
  of the three eager paths"*, the very thing that proposal's headline change exists to remove. The
  recur sequence had been paying the full `O(list)` eager resolve the whole time.

  **How it was found.** Two wrong diagnoses first, both from reasoning instead of measuring: that
  the program side never produces `List` (a grep for the literal enum variant missed
  `recur_source_trace`'s indirect path), and that both recur macros already called it (two call
  sites seen, assumed one per macro — both are recur-*tile* variants). The measurement that settled
  it: the site's `"input"` binding was byte-identical to the enclosing sequence's `pieces`
  argument, `selected_len = 441` where metadata is a constant 41.

  **Verified:** `a_recur_sequence_site_commits_to_its_source_list_metadata` (`raster/tests/
  recur_draft.rs`) asserts `payload_kind == List` and `selected_len == 41` through the macro rather
  than a hand-built fixture — and reverting the one-line fix makes it fail with the production
  symptom (`left: Raw, right: List`), so it is not tautological. `prompt-prepare` regenerated its
  commitment honestly (exit 0) and re-audited. Error message corrected too: it named `call_recur!`
  on a path reachable from either macro.

  **Cost:** every recur-sequence site's recorded binding changes, so `input_source_commitment`,
  the step record, the trace leaf and the fingerprint all move. Any existing `commit.bin` for a
  program using `call_recur_seq!` must be regenerated.

- **A window step is not where the CFS expects it** — newly reached, and the current blocker:
  `checks/cfs.rs` → `Step coordinates are not in expected next coordinates`, two steps into a
  window. The injector places its tamper by trace index, so the regenerated commitment moved the
  window to a different part of the trace; whether this is a property of that region or of the new
  window's opening position is the first thing to establish.

- **Why a `SequenceStart` needs no storage evidence at all** (recorded because an earlier draft of
  this file claimed the opposite). A step with no `StorageRoots` never proposes a root: the guest
  returns its own carried frontier unchanged, and every step that *does* declare roots must match
  it (`Execution-step storage root before does not match current storage root`). So a sequence step
  is transparent to the storage chain — it cannot alter the root because it is never asked for one,
  and giving it roots would be *worse*, handing a prover somewhere to propose one.

  Its arguments are authenticated in three places, none of them a storage witness on the sequence
  step: `input_source_commitment` binds the argument list to the record and the record is
  fingerprint-bound to `commit.bin`; `verify_step_record_inputs` holds those arguments to the CFS;
  and the **consuming** step — which has roots — proves the coordinates are in the store and the
  selection resolves. `checks/cfs.rs`'s `verify_sequence_scope_parent` plus `assert_record_in_trace`
  pin "argument `i` of the parent" to the parent's own record. The obligation is discharged where
  consumption happens, which is the only place there is an authenticated root to check against.

  Open nuance, untraced: a recur sequence *iteration*'s `SequenceStart` returns early from
  `verify_step_record_inputs` into `verify_recur_iteration_chunking`, so the generic CFS binding
  check is skipped there in favour of the recur rules. Whether those cover the same ground is worth
  its own look.

- **A recur site's source is recorded as a raw payload, not list metadata** — newly reached, and
  the current blocker:

  ```
  checks/cfs.rs → Recur site start StepRecord { exec_index: 735,
      sequence_id: "merge_prompt_piece", coordinates: CfsCoordinates([3, 0]),
      kind: SequenceStart { .. } } must commit to list metadata, not a raw payload
    left: Raw    right: List
  ```

  `authenticated_source_len` requires the site's `"input"` binding to commit to the `0x0A`
  `(len, elements_root)` record. That strictness is load-bearing, not incidental: it is what makes
  `L` *authenticated* rather than index-trusted, and it is the check that refuses the forged
  `len = 0` sweep `lazy-list-recur` exists to close. A `Raw` payload cannot satisfy it even in
  principle — `decode_list_metadata_len` parses the `0x0A` form and a raw payload is not one.

  **Not yet diagnosed.** An earlier draft here claimed the program side never produces `List`,
  from a grep for the literal enum variant in `raster-macros`/`raster`. That grep was too shallow
  and the claim is **wrong**: `raster/src/input.rs`'s `recur_source_trace` builds the binding from
  `stored_list_metadata`, whose commitment is `List`-kinded, and *both* recur macros call it
  (`raster-macros/src/recur.rs:745` and `:870`, each with a comment citing `lazy-list-recur` §2).
  So §2 is implemented and the site start should already record `List`.

  What is established: the failing record is the recur **site** start (`[3, 0]` is main's item 3 →
  `merge_round` → its child 0, the `call_recur_seq!` site; iterations are `[3, 0, i]`), the
  `"input"` binding is present, and its recorded `payload_kind` is `Raw`. Why a binding built by
  `recur_source_trace` arrives as `Raw` is the open question — measure the recorded binding rather
  than reason about it, which is what the wrong claim above cost.

  A related correction: an even earlier draft guessed this was specific to a produced-list source
  because "the site at `[2, 0]` passed". That evidence does not exist — `[2, 0]` passed only as the
  site-*close* `Exec` step, and `authenticated_source_len` runs only on `SequenceStart`.

  **The tests still hand-build the shape.** `seed_recur_source` inserts
  `metadata.selected.commitment` directly rather than exercising `recur_source_trace`, so whatever
  is happening between that function and the recorded trace is not covered by any recorder test.

  **Directions** (not picked): build the site's `input` binding from a metadata selection in
  `call_recur_seq!`/`call_recur!`, so the recorded commitment describes what the site actually
  consumes; or patch the binding in the recorder when it emits the site `Start`, where
  `recur_source_len` already holds the metadata and where `input_source_commitment` is computed
  over the same (patched) input. The first is what `lazy-list-recur` §1–§2 describes; the second is
  smaller but makes the recorded binding differ from what the program published.

## 3. What this is not

- **Not a soundness hole.** It fails closed: nothing here lets a receipt exist against an honest
  prover. It is a completeness defect, and in an optimistic protocol a completeness defect on the
  dispute path is still a lost dispute.
- **Not window geometry.** `fraud-window-trace-ends` is closed; its head and tail cases were about
  the window's *length*. The divergence here is mid-trace, the window is a full 32 items, and the
  failure is after the window is built.
- **Not the `witness_record_inputs` panic.** That one is fixed (`0a71d10`) and sits earlier, inside
  `verify()`. This is the next site in the same path — see §5's first direction.
- **Not the `revealed_items` asymmetry.** Byte 5000 flipping clean is
  `fraud-window-trace-ends` §4 restated: the audit reads `revealed_items` only by `.len()`, and its
  contents are bound on the transition-guest path instead. Still true after the `v2` header work,
  which added `window_size` but never made the guest open `revealed_items_commitment`. A separate
  observation, recorded here only because the same sweep surfaced it.
- **Not `--no-auth`.** That mode emits no trace, so there is nothing to prove or refute.

## 4. What it costs

For `prompt-prepare` — a real program, not scaffolding — the fraud-proof path currently reaches a
verdict and then aborts. What an operator sees is a stack trace at exit 101, not a receipt.

`chain-io-commitment` §3's dispute protocol is not built yet, so nothing settles on this today.
When it is, this is the difference between condemning a cheat and timing out.

## 5. Directions

Shapes only; none is picked. The first two are diagnostics, and both should run before any fix is
designed — the third and fourth differ by which one is true.

- **Bisect against `0a71d10`.** `witness_record_inputs` runs inside `verify()`, so it fires before
  `prove()`. Check out `63980a9`, regenerate a v1 commitment, flip a mid-fingerprint byte: if it
  dies earlier with `"Failed to resolve active sequence invocation"`, this is the next link in one
  chain and the chain is longer than `fraud-window-trace-ends` §5 assumed. Cheap, and it decides
  whether to expect more sites behind this one.
- ~~**Name the failing step.**~~ **Done 2026-09-16** — see §2. Neither hypothesis held; the answer
  was a single binding whose recorded source coordinate names a step that never executed and has
  the wrong arity for its position. The diagnostic patch (report every binding rather than
  panicking on the first; check the source against the trace) is not committed — re-apply it in
  `build_storage_selection_witnesses` (`run.rs:737`) if needed.
- **Make the runtime address storage the way the recorder does** (§2b). In
  `reserve_execution_coordinates`, the recur branch must apply only where it is correct — a recur
  *tile*, whose iteration genuinely is one tile execution and which pushes no `SequenceFrame`.
  Inside a recur *sequence* iteration the right frame is already on the stack; prefer it. Two
  shapes: give `RecurFrame` a site kind, or give it an `iteration_open` flag set by
  `enter_recur_sequence_iteration`/`exit_recur_sequence_iteration` — the latter names the exact
  condition. Either way `next_iteration_index` must stop being advanced by body tiles, since
  `enter_recur_sequence_iteration` reads that same counter and its iteration numbering drifts too.
  **Costs:** every storage coordinate inside a recur-sequence body moves, so recorded traces and
  every commitment over them change.
- **Or reconcile the two maps rather than the two schemes.** `TraceRecorder.storage` and
  `THREAD_STORAGE` are separate `StorageManager`s populated by different paths. Whether they are
  *meant* to agree key-for-key is a design question this issue does not settle; if they are, the
  divergence is a bug in whichever path is wrong, and if they are not, then resolving a recorded
  `StorageRef` against the recorder's map is the mistake and the fraud path needs the runtime's.
- **Decide what a source coordinate with no stored object means.** If the trace can legitimately
  record one, evidence construction must tolerate it — and `selected_len == 0` (`:751`) is already
  a precedent for skipping a binding, so the question is whether this is a second legitimate skip
  or a recorder defect that should never have emitted the binding. Costs: skipping wrongly hands
  the guest a window missing a witness it needs, which moves the failure into the zkVM.
- **Return an error rather than panicking.** `run.rs:767` is `panic!` inside `unwrap_or_else`, and
  `prove()` already sits in a `Result`-returning path. Strictly an improvement in reporting and
  strictly not a fix: the proof still cannot be built. Worth separating from whatever closes the
  underlying gap, so that "no longer crashes" is not mistaken for "now provable".
- **Reconstruct the witness from the trace instead of live storage.** The recorded
  `StorageData` carries the coordinates, commitment, selector and selection commitment; only the
  payload is missing. If that is derivable from the trace the auditing process already holds, the
  live map stops being the authority. If it is *not* derivable, the honest consequence is worth
  stating plainly: a challenger would need data only the accused holds, which is a
  data-availability dependency — see `raster-prover::availability`, which names that assumption.

## 6. Reproducing

```bash
cd raster-inference/prompt-prepare
cargo raster run --input input.json --input-manifest input_manifest.json \
  --commit /tmp/c.bin --fraud-proof-window-size 32

# The supported path: the built-in injector, which corrupts one executed step.
cargo raster run --input input.json --input-manifest input_manifest.json \
  --commit /tmp/fraud_demo.bin --fraud-proof-window-size 32   # see run.rs:528
cargo raster run --input input.json --input-manifest input_manifest.json \
  --audit /tmp/fraud_demo.bin
```

Or by hand, on the packed fingerprint rather than the header:

```bash
python3 -c "
import pathlib
b = bytearray(pathlib.Path('/tmp/c.bin').read_bytes()); b[100] ^= 0x01
pathlib.Path('/tmp/mid.bin').write_bytes(bytes(b))"
cargo raster run --input input.json --input-manifest input_manifest.json --audit /tmp/mid.bin
```

Two adjacent outcomes worth not confusing with detection, both reachable from the same sweep:
flipping byte 0 changes `bits_packer` and is refused by `validate()`; flipping with `^ 0xFF`
rather than `^ 0x01` clears postcard's varint continuation bit and is refused by the decoder.
Both exit non-zero without a fingerprint ever being compared.

`prompt-prepare/fraud_demo.bin` is left untracked in the tree from the reproduction;
`prompt-prepare/commit.bin` is the honest regenerated one and was not modified.
