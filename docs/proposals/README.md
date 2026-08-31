# Proposals — status and dependencies

Index of `docs/proposals/`. Last reviewed 2026-08-16.

Each proposal's own `Status:` line is the source of truth; this file collects them and records
where that line disagrees with the code or the git history. Where they disagree, the
disagreement is listed rather than silently resolved — a stale header is a fact about the
document, and correcting it is the author's call.

## Status at a glance

| proposal | status | landed | outstanding |
| --- | --- | --- | --- |
| [`program-start`](./program-start.md) | **implemented** ⚠️ header says `proposed` | `ProgramStart` replaces `SequenceStart(main)` + `Entrypoint` | header is stale — `program-end.md` and `program-identity.md` both cite it as implemented |
| [`program-end`](./program-end.md) | **implemented** (2026-07-16) | authorized output as a boundary step; `checks/entrypoint.rs` | — |
| [`program-identity`](./program-identity.md) | **implemented** (2026-07-22) | `program_commitment` over the `ProgramDefinition` frame; registry-resolved image ids | — |
| [`program-chain`](./program-chain.md) | **partly implemented** ⚠️ header says `proposed` | stage checkpoints exist — `chain-fraud-proof` is implemented *on top of them* | header is stale; commits `7806b27`, `0a98335` |
| [`chain-fraud-proof`](./chain-fraud-proof.md) | **implemented** (2026-07-26) | whole-chain fraud proof; compact commitment header + slice proof | — |
| [`bounded-collections`](./bounded-collections.md) | **implemented** (2026-07-29) | `List<T>` / `Block<T>`; `Materializable`; `0x09` list handle — phases 1 and 2, the only two it defines | — |
| [`dynamic-index-selection`](./dynamic-index-selection.md) | **phases 1–3 implemented** (2026-07-30) | `BoundIndex`, citations, `verify_bound_index_bindings`, `into_ref!` | option 2 on the width check, left to be argued on its own terms |
| [`sequence-grammar-closure`](./sequence-grammar-closure.md) | **phase 1 implemented** (2026-07-30) | `clone!`, `into_ref!`, the `.clone()` backstop | **phase 2** — inverting the default so an unrecognized form is an error, not `Inline` |
| [`authoring-skill-and-tooling`](./authoring-skill-and-tooling.md) | **half landed** ⚠️ header says `proposed` | the skill: `.claude/skills/raster/` (commit `d78c9a5`) | `cargo raster check` — `raster-cli` has only `run` and `tile` |
| [`zkvm-dry-run`](./zkvm-dry-run.md) | proposed 2026-08-17 — **rev 2**, narrowed from `guest-replayability-check` | — | whole; `cargo raster run --dry-run` (executor replay of every tile step, no proving) + a corrected proof-cost model. Rev 2 dropped the static/cross-compile tiers to `authoring-skill-and-tooling` |
| [`draft-provenance`](./draft-provenance.md) | proposed (2026-07-30) | — | whole |
| [`incremental-draft-witness`](./incremental-draft-witness.md) | **implemented** (2026-08-15) | `input::AppendFrontier`; `DraftWitnessField` split off `DraftFieldValue`; frontier-based `apply_draft_ops` / `draft_root_from_witness`; live frontier in the runtime (the host recomputed the whole root per push too). Trace-format hard break; input fixtures and `program_commitment` unmoved, guest image ids move (`raster-core` is linked into them) | §5 not done by design — the witness still carries a full `SchemaNode` per step and a set-once field's whole value rather than its root; acceptance run on `raster-inference` not yet reported |
| [`loop-carried-state`](./loop-carried-state.md) | proposed (2026-07-30) | — | whole; see the note under *Dependencies* |
| [`lazy-list-recur`](./lazy-list-recur.md) | **phases 1–6 implemented** (2026-08-13/14) | metadata payload, `ListCursor`, driver-level chunking + range descent, per-item bindings, §5 journal facts, rules 1–7 + S1/S3/S4 enforced | **rule 8 unimplemented** (no `ListRange` cross-check) and S2 unenforced; the peak-RSS acceptance benchmark was never run — see §Outstanding at implementation |
| [`recur-progress-commitment`](./recur-progress-commitment.md) | **rev 2 implemented** (2026-08-14) | `recur_progress.rs`, the trace `recur_control` bit, site `Start`/`End` events, recorder stamping, guest advance-and-compare — recorder and guest agree on every commitment | mid-loop window seeds, split out as `window-seed-reconstruction` |
| [`paged-bytes`](./paged-bytes.md) | **rev 3 implemented** (2026-08-14) | `Bytes<P>` / `BytesPage`, tag `0x0B`, `rindex03` hard-break, `InterfaceDecl.schema_hash`, geometry audit, `select!` byte→page conversion, ranged `Read` | Gate 2/3 still open (`ListRange` cross-check, selection↔replay bind); no `pages!` sugar |
| [`recur-sequence-break`](./recur-sequence-break.md) | proposed 2026-08-13 | — | whole; blocked on `recur-progress-commitment` rev 2. Weakens `lazy-list-recur` S4 to a prefix/terminal split |
| [`window-seed-reconstruction`](./window-seed-reconstruction.md) | proposed 2026-08-14 | — | whole; small. Without it a fraud-proof window opening mid-loop is rejected — the design `recur-progress-commitment` explicitly refused, reinstated by an unfilled parameter |
| [`carried-state-channel`](./carried-state-channel.md) | proposed 2026-08-07 — **enhancement** | — | deliberately deferred until a second component is ready |
| [`trace-event-vocabulary`](./trace-event-vocabulary.md) | **implemented** (2026-08-13) | `RecurSequenceIterationStart`/`End`; the naming rule and vocabulary table on `TraceEvent` | — |
| [`chain-repeat`](./chain-repeat.md) | **implemented** (2026-08-27) | `[[chain.repeat]]` with an authorized trip count (literal or stage-produced); `[chain.input]` named + indexed externals; `ChainShape` in the chain commitment; `ChainFaultKind::Shape` **added beside `Link`** (not a restoration of the removed `Execution`). `raster-inference`'s 35 `prefill_prepare_aux` stages are now one block, expanding to the identical 74 stages. Chain-commitment format break — `spec_digest` moves every recorded digest, and **closes S1** (`chain_spec_commitment`), still recorded as open in `chain-fraud-proof` and `chain-io-commitment` | external (`{ input = ... }`) counts; `while` mode; collapsing `prefill_range`, which needs the §7 donor rewrite plus a two-block split |
| [`unauthenticated-execution`](./unauthenticated-execution.md) | **implemented** — v1 2026-08-19, v2 2026-08-20, v3 2026-08-20 | runtime `AuthMode` (`raster-runtime/src/auth.rs`); `select!` dispatched on base provenance, so storage sources stay lazy; drafts keep field values and drop commitments; recur full; `cargo raster run --no-auth`; no trace emitted, so a trace commitment is structurally impossible; profiling refused; RAS-203a landed. v3: `cargo raster chain run --no-auth` — all-or-nothing, no chain-commitment, own runs root; plus a storage-backed base indexed by a tile-produced value, which §5.3/§5.4 left uncovered and which stage 1 of `raster-chain-inference` hit immediately. **6.6× on `hello-tiles`**, both modes value-identical end to end | typed `Schema::Partial` to remove the remaining serialize per draft op — deferred, needs a measurement on a draft-heavy program. Mixed-posture chain policy (on-demand per-stage commitment) still out of scope — §10; the cheap-stage half of §10 is now [`chain-stage-execution`](./chain-stage-execution.md) |
| [`chain-stage-execution`](./chain-stage-execution.md) | **partly implemented** (2026-08-21) | §2–§4: `cargo raster chain run --no-auth --stage <name> [--run <dir>]` — one stage re-run in place, producer commitments rehydrated from `output.bin` via the existing `collect_output`, downstream stage dirs invalidated in spec order, `latest` pointer, spec-validity (`from` ordering) check moved ahead of execution. Authenticated path untouched. Verified end-to-end on a three-stage chain (`tests/chain_stage_cli.rs`, 7 tests — middle-stage re-run, multi-stage invalidation, stage-by-stage rebuild converging on the whole-chain result), for which it also supplies `examples/chain-example`, the chain fixture `program-chain` implementation order step 5 called for and never got | §1 — promoting the mode from `--no-auth` to a command, and the `chains-dry/` rename. **Blocked on naming**: `dry-run` reverses `unauthenticated-execution` §Naming *and* takes the term `zkvm-dry-run` §3 reserves; `unauth` costs one line and no collision. Untested: posture isolation. ⚠️ **§5's "authenticated path untouched" no longer holds** — [`chain-io-commitment`](./chain-io-commitment.md) lifted the `requires = "no_auth"` gate on `--stage`/`--run`, because its stated reason (what a chain commitment means when stages were committed at different times) does not arise when the per-stage commitment is a dispute artifact rather than a checkpoint field. An authenticated `--stage` run writes `commit.bin` and leaves the chain-commitment alone |
| [`program-manifest`](./program-manifest.md) | proposed 2026-08-26 | — | whole; one `Raster.toml` grammar (`[program]` xor `[chain]`, one parser), `[program]` **mandatory** — reverses `program-identity` §Manifest slimming's "optional with derived defaults", which in practice means **no program in the tree authors the manifest its identity is computed over**; identity artifact renamed `program.bin` → `<program.name>.bin`; chain membership via `version.chain = true` / `chain = "<path>"` / per-parameter `source = "chain"`. Costs a one-time `program_commitment` move for all four in-tree projects |
| [`chain-io-commitment`](./chain-io-commitment.md) | **partly implemented** (2026-08-27) | §1 checkpoint narrowing + the journal work under it: `OutputAuthorization::Established { output_commitment }` (the value `verify_program_end` already checked and discarded), `window_is_terminal` derived at `Init`, `TraceVerifier::terminal_window`, `StageCheckpoint` loses `trace_commitment_digest` so **both postures write a real `ChainCommitment`**, `chain audit` loses its commitment-binding check, `detect_execution_fraud` → `detect_output_fraud`, `ChainFaultKind::Execution` removed. Two forced decisions: `--no-auth` **degrades** (unresolvable identity drops the commitment, run proceeds) rather than reversing `unauthenticated-execution` §10; and `chain fraud-prove` now emits a terminal-window **evidence** receipt so removing `Execution` is not a capability regression | **§3 dispute protocol not built** — `StageChallenge`, admission checks, `ChallengeFraudJournal`, `chain challenge`/`respond`/`challenge-verify`; inert without a settlement clock, so it gates use, not design. Costs: execution fraud is condemned by silence + timeout rather than by a self-contained receipt. Settlement contracts, artifact DA and bonding are **assumed planned infrastructure** (§Assumed infrastructure); attacks reducing to them are marked `[infra]` and not treated as blockers. The one in-repo blocker is a **hard dependency on `window-seed-reconstruction`** — a terminal window opening mid-recur is rejected today, which makes recur-heavy stages unchallengeable |
| [`artifact-inspection`](./artifact-inspection.md) | **implemented** (2026-08-31) — rev 2 | `cargo raster show` + `--show-output` on `run` / `chain run`; `raster-runtime/src/reader.rs`, `RasterValue` and the bounded walk in `raster-runtime/src/input.rs`, `raster-cli/src/commands/show.rs`; 12 + 9 tests. Nothing in `raster-core` touched | **§2 structural fallback deferred** (a missing `.rindex` errors and names the path), and with it §3's `0x09` stored-root-vs-elements cross-check — whole-artifact integrity (payload root vs `.rindex` root) *is* reported on every read. `RasterValue` departs from §1's sketch: truncation flags on `Str`/`Bytes`/`Map`, `Int` carries its width for §4.1's `353u64`, an `Elided` variant for the depth limit, and no `Float` (the encoder has none). See §Implementation record. Original scope: `cargo raster show <artifact>` — decode a raster payload back into a typed, structured value (one command over `output.bin`, a stage artifact and an external `*.rastered`). The decoder already exists and is exercised on every selection (`RasterNodeKind::Leaf` carries `type_name`; `parse_leaf_value` / `tree_value_from_raster_node`) — it is all `pub(crate)` in `raster-runtime`, so the work is exposing it, truncation limits, and rendering. Today the only way to read an artifact is `strings(1)`. Rev 2 resolves open question 3 into **§6 `--show-output`** on `run` / `chain run` (opt-in, off by default, final stage only, same renderer and limits as `show`; with `chain-stage-execution`'s `--stage` this also covers a middle stage, since the stage you re-ran is the last one that ran). Rev 2 also **rejects `--select`** (`show` reads a file, it does not query one; `--format json` + `jq` covers it, and every selector surface is another place the path grammar can drift from `select!`'s) and **rejects `chain show <stage>`** (sugar over a path, bought at the cost of a second place chain run-resolution can drift from `chain-stage-execution`'s), and **defers** the structural fallback for a missing `.rindex` together with the `raster-core` `parse_subtree_root` walk/hash split under it — `--show-output` never needs it, so the first cut touches only `raster-runtime` + `raster-cli`. Adds §4.1: the text format is `Debug`-shaped but renders structs anonymously, because `RasterNodeKind::Struct` records field names and **no struct type name** — exact `Debug` still needs the rejected link-the-program-crate alternative |

## Dependencies

```
program-start ──┐
program-end ────┼──► program-identity ──► program-chain ──► chain-fraud-proof
                │         (impl)             (partial)   │      (impl)
                └── (impl)   │                           │        ▲
                             │                           └──► chain-repeat ──┘
                             │                                (proposed)
                             └──► program-manifest ◄── also reorganizes the [chain]
                                    (proposed)             table chain-repeat extends
                                        also borrows the authorized-value rule from
                                        dynamic-index-selection (impl)

chain-fraud-proof (impl) ──► chain-io-commitment ◄──── window-seed-reconstruction
   reuses the window/slice        (proposed)   hard dep    (proposed)
   binding; disagrees on one      ▲            — a terminal window opening mid-recur
   checkpoint field               │              is rejected, so recur-heavy stages
                                  │              cannot be challenged
   chain-stage-execution ─────────┘
     (partial) supplies the determinism fact and the --stage machinery;
     its §5 refusal to touch the authenticated path is lifted there

bounded-collections (phases 1-2 impl)
        │
        ├──► lazy-list-recur ◄──── recur-progress-commitment ──► recur-sequence-break
        │         │   ▲              (rev 2 impl) │                (proposed)
        │         │   │                           └──► window-seed-reconstruction
        │         │   │                                     (proposed)
        │         │   │
        │         │   └── dynamic-index-selection (impl) — citations survive materialization
        │         │
        │         └──► paged-bytes ── also needs ──► program-identity (impl)
        │
        └──► loop-carried-state ····► carried-state-channel ····► (drafts: needs a
                  (proposed)            (enhancement, later)        creation event first)

sequence-grammar-closure (phase 1 impl) ◄──► draft-provenance
        phase 2 and draft-provenance refine each other; neither blocks the other

authoring-skill-and-tooling (half landed) ····► zkvm-dry-run
        owns RAS-206/208; the dry run is their first enforcement       (proposed)

lazy-list-recur (impl) ─ same fix, write side ─► incremental-draft-witness
                                                         (impl)
                                                          ▲
                        window-seed-reconstruction ·······┘
                             (proposed)          shared frontier/seed mechanism

unauthenticated-execution ····► incremental-draft-witness (impl) + lazy-list-recur (impl)
        (proposed)              v1 defers Draft/recur because those own what a draft
                                and a recur iteration would *mean* with no storage
        ├── proposes RAS-203a into authoring-skill-and-tooling (half landed)
        ├── suspends, in this mode only, the authorized-index rule from
        │   dynamic-index-selection (impl)
        ├····► chain-stage-execution (proposed) — the cheap-stage half of §10:
        │      per-stage re-execution, unattested only, over program-chain (partial)
        └····► still unwritten: on-demand per-stage commitment for a contested
               stage, and what a mixed-posture chain commitment means — §10

program-end (impl) ──► artifact-inspection (impl)
        defines output.bin;   `cargo raster show` reads it back. Non-blocking:
        the decoder exists in raster-runtime, unexported. chain-stage-execution
        (partial) is what makes the absence acute — re-run one stage, then have
        no way to see what it produced.
```

`──►` blocking. `····►` recommended, not blocking.

### The blocking edges, stated

- **`paged-bytes` → `lazy-list-recur`.** Three gates, per `paged-bytes.md`. Gate 1 (§1–§4, sweep
  at all) is **satisfied**. Gate 2 (§5, call a sweep *complete*) is satisfied for element recur
  and **partly** for chunked: rule 8's `ListRange` cross-check is unimplemented, so a chunked
  sweep's coverage currently rests on the replay journal alone rather than on a folded proof.
  Gate 3 — the storage-selection-to-replay binding (`paged-bytes` §3.3) — is untouched and out of
  scope for both.
- ~~**`lazy-list-recur` §5 → `recur-progress-commitment`.**~~ **Satisfied 2026-08-14.** The
  carrier landed with revision 2, so §5's rules now bind across window boundaries rather than
  only in a window containing iteration 0.
- **`window-seed-reconstruction` → nothing; it unblocks mid-loop windows.** Until it lands, a
  window opening inside a live loop is rejected because its seed is never reconstructed from the
  trace prefix — which is the "refuse to open mid-loop" design `recur-progress-commitment`
  §Problem explicitly rejected, arrived at by an unfilled parameter rather than by choice.
- **`recur-sequence-break` → `recur-progress-commitment` rev 2.** Not merely ordered after it:
  the break bit rides on the `recur_control` trace field rev 2 introduces, and S4′ rewrites the
  `close_site` rule rev 2 implements. Landing it first would mean implementing both halves of
  that proposal anyway, in the wrong order.
- **`chain-fraud-proof` → `program-chain`, `program-identity`.** Already satisfied; noted
  because it is why `program-chain`'s `proposed` header must be stale.
- **`chain-io-commitment` → `window-seed-reconstruction`.** Its challenge is a *terminal-window*
  receipt, and for a recur-heavy stage that window very often opens inside a live loop — which is
  rejected today. So the mid-loop gap stops being a fraud-proving inconvenience and becomes a
  soundness-adjacent one: a claimer could pick such a program and be unchallengeable. Ship
  together. This is the second consumer of `window-seed-reconstruction`, which until now was
  wanted only by `recur-progress-commitment`'s own window model.

### The non-blocking edges, stated

- **`incremental-draft-witness` → nothing; it is independent of `draft-provenance`.** Both touch
  drafts and neither blocks the other: `draft-provenance` is about `finalize` severing a
  provenance chain, this is about the witness carrying O(N) elements to prove one root. It does
  share a mechanism with `window-seed-reconstruction` — a frontier is what a window opening
  mid-draft would need as its seed — so whichever lands first should decide whether the frontier
  lives in the witness or in `TrackedDraftState`. That is the first entry in its
  §Uncertainties, not a blocking edge. **Decided at implementation: the witness owns it.**
  `TrackedDraftState` is untouched, so `window-seed-reconstruction` can still move the frontier
  there if it wants the smaller-but-more-coupled form.
- **`loop-carried-state` → `carried-state-channel`.** `loop-carried-state` §2 proposes a
  `TrackedStateRoot` map "mirroring `active_drafts`", which would reproduce that map's
  window-open gap. It should extend the channel instead. Neither proposal blocks the other; the
  ordering only decides whether the trace format breaks once or twice.
- **`zkvm-dry-run` → `authoring-skill-and-tooling`.** Non-blocking in both directions. The dry
  run depends on nothing that proposal delivers; it is the first *enforcement* of RAS-206
  (determinism) and RAS-208 (replay size), both of which that proposal marks `[none]` and its
  §6 leaves to "the zkVM replay itself". Rev 2 pushed the static-lint half back to
  `cargo raster check` rather than duplicating it.
- **`carried-state-channel` → drafts.** Folding `active_drafts` in needs the trace to express
  draft *creation* first: `create_draft` (`raster-runtime/src/storage.rs:817`) emits no step, so
  an absent map entry is legitimate today and `checks/drafts.rs:69` is permissive for that
  reason.

## Ready to implement now

1. **`lazy-list-recur` rule 8** — the one *missing check* left in an otherwise implemented
   proposal. Without the `ListRange` cross-check, a chunked sweep's coverage rests on the replay
   journal alone, which §6 is explicit is "a binding, not an authority". Small, and it is
   `paged-bytes`' second gate. Take its S2 pairing check and the peak-RSS acceptance benchmark
   with it — see that proposal's §Outstanding at implementation for the full list.
2. **`window-seed-reconstruction`** — small (three files, one a single line) and it restores the
   property `recur-progress-commitment` was built for: a fraud window that opens mid-loop.
   Currently such a window is rejected.
3. **`sequence-grammar-closure` phase 2** — independent of the above; `draft-provenance` argues
   one row of its classification table and can be taken with it or separately. A nested call
   macro in another call's arguments is now rejected at expansion
   (`raster-macros/src/lib.rs`, `reject_nested_call_macros`), which is an instance of this
   proposal's rule that arrived early via a different failure.
4. **`authoring-skill-and-tooling`'s second half** (`cargo raster check`) — independent, and it
   is the enforcement surface for rules the type system cannot express.
5. **`zkvm-dry-run`** — the only thing that decides "does this tile actually run in the zkVM"
   without proving, and the first enforcement RAS-206/208 have ever had. Independent. Not quite
   free: `Replayer::replay` rejects a receiptless execution (`replay.rs:106`), so a
   `TileExecutionResult.journal` field and a sibling `dry_run` method come with it — see §4.
   Its §5 also reports that `calculate_proof_cycles` over-counts multi-segment executions and
   that `Estimate`/`Prove` put different cycle quantities in the same field.

`trace-event-vocabulary` landed 2026-08-13 and was amended 2026-08-14: the trailing
`RecurTileExec` / `RecurSequenceExec` became `…End` and gained `…Start` halves, so a recur site
now brackets its iterations the way a sequence brackets its items. Variant indices were preserved
(rename + append), so the event enum contributed nothing to the format break.

## Blocked or deliberately deferred

- **`paged-bytes` Gate 2/3** — format and addressing landed; chunked-sweep `ListRange`
  cross-check and the selection↔replay bind are still open.
- **`carried-state-channel`** — deferred by choice until a second component is ready. Waiting
  is free: adding a component to it breaks the trace format exactly as much as adding a field
  does, so nothing is saved by adopting it early.
- **`loop-carried-state`, `draft-provenance`** — not blocked, not scheduled.
- **`chain-io-commitment`** — steps 1–3 (the journal's output value, the terminality pin, the
  host plumbing) are landable now and leave the current protocol working. The checkpoint
  narrowing at step 4 is the point of no return. One in-repo blocker:
  `window-seed-reconstruction`, without which recur-heavy stages cannot be challenged at all.
  The settlement/DA/bonding dependencies are assumed planned and deliberately not treated as
  blockers — the dispute protocol is inert without them, so they gate *use*, not *design*.

## Known open gaps not owned by any proposal

- **The storage-selection-to-replay binding** (`paged-bytes` §3.3). Until it lands, "each tile
  consumed the value at the index it claims" is unproved — a trace whose item proof selects `B`
  while the replay input decodes to `C` is accepted. Named as a dependency by both
  `lazy-list-recur` and `paged-bytes`; owned by neither. Any soundness claim about recur should
  use `lazy-list-recur`'s claim table rather than the phrase "authenticated iteration".
- **`next_expected_coordinates` at a fresh `Init`.** Unconstrained, so a window's first step's
  coordinates are not held to the CFS. Unlike carried state it wants *derivation* (from the
  first step's own coordinates plus the CFS), not a carrier. Recorded in
  `recur-progress-commitment` §Problem.
- **No loop-carried slot is both readable and incrementally committed.** `state` is readable and
  re-committed whole every iteration; `output` pays only its increment and holds no value to
  read. A bounded accumulator written slice by slice therefore costs `2 · N · |state|` to carry
  through `N` iterations, and a data-dependent `Break` can only be decided from the expensive
  slot — which is also the unchecked one. Written up as
  [`docs/issues/recur-accumulator-slots.md`](../issues/recur-accumulator-slots.md); adjacent to
  `loop-carried-state` (recur *sequences* carrying a `List` by reference) but not covered by it,
  since a recur *tile* body has no `select!` and a reference would be materialized anyway.

Gaps are collected in [`docs/issues/`](../issues/README.md) once they are reproducible from the
code; that directory's README states the issue-versus-proposal split.
