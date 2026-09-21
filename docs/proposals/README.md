# Proposals — status and dependencies

Index of `docs/proposals/`. Last reviewed 2026-09-16.

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
| [`storage-write-cost`](./storage-write-cost.md) | proposed 2026-09-21 — **measured** | — | whole; a storage `append` costs 194–363 µs and the frontier append that records it is **0.3%** of that. Two terms dominate and both are paid by *every* storage write: `current_root()` recomputes from a **cloned** frontier and is called twice per append (32–55%), and the coordinate index is `INDEX_BITS = 256` deep (44–68%). Term 1 is a pure caching fix — no format, trace or witness change — and `store_root_before` is by construction the previous append's `store_root_after`. Term 2 is raised, not proposed: it moves the index root and every commitment. Sequenced **before** the draft proposals, whose wins are smaller than this one |
| [`incremental-draft-materialization`](./incremental-draft-materialization.md) | proposed 2026-09-21 | — | whole; the *payload and index* half of what [`incremental-draft-witness`](./incremental-draft-witness.md) did for the digest. Verified: `AppendFrontier`'s root already equals the list root, and `finalize` then rebuilds it — **every element is hashed twice**. Two blockers found, both in layout: `RasterNode.offset` is absolute and struct fields are laid out sequentially, so appending to a non-last field shifts everything after it. Also carries the lifetime change — the wrapping sequence closes a draft, `finalize` leaves the grammar — which subsumes [`draft-provenance`](./draft-provenance.md), answers both of [`recur-deferred-finalize`](./recur-deferred-finalize.md)'s open questions, and gives [`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md) the trace step it is missing |
| [`draft-map`](./draft-map.md) | proposed 2026-09-21 — **problem statement only** | — | whole, and deliberately no mechanism. Draft→draft transformation: the appends are already attested per `TileExec`, but coverage is not — nothing binds `len(B)` to `len(A)` or `B[i]` to `A[i]`. The same missing join as [`selection-unbound-from-execution`](../issues/selection-unbound-from-execution.md). Blocked on a question with no answer yet: a recur's bound `L` is authenticated by a `0x0A` selection over a *stored* list, and an open draft has neither |
| [`incremental-draft-witness`](./incremental-draft-witness.md) | **implemented** (2026-08-15) | `input::AppendFrontier`; `DraftWitnessField` split off `DraftFieldValue`; frontier-based `apply_draft_ops` / `draft_root_from_witness`; live frontier in the runtime (the host recomputed the whole root per push too). Trace-format hard break; input fixtures and `program_commitment` unmoved, guest image ids move (`raster-core` is linked into them) | §5 not done by design — the witness still carries a full `SchemaNode` per step and a set-once field's whole value rather than its root; acceptance run on `raster-inference` not yet reported |
| [`loop-carried-state`](./loop-carried-state.md) | proposed (2026-07-30) | — | whole; see the note under *Dependencies* |
| [`lazy-list-recur`](./lazy-list-recur.md) | **phases 1–6 implemented** (2026-08-13/14) | metadata payload, `ListCursor`, driver-level chunking + range descent, per-item bindings, §5 journal facts, rules 1–7 + S1/S3/S4 enforced | **rule 8 unimplemented** (no `ListRange` cross-check) and S2 unenforced; the peak-RSS acceptance benchmark was never run — see §Outstanding at implementation. Rule 8 and the claim table's last row are now [`selection-unbound-from-execution`](../issues/selection-unbound-from-execution.md), **top priority**; S2 and the missing evidence are not |
| [`recur-progress-commitment`](./recur-progress-commitment.md) | **rev 2 implemented** (2026-08-14) | `recur_progress.rs`, the trace `recur_control` bit, site `Start`/`End` events, recorder stamping, guest advance-and-compare — recorder and guest agree on every commitment | mid-loop window seeds, split out as `window-seed-reconstruction` |
| [`paged-bytes`](./paged-bytes.md) | **rev 3 implemented** (2026-08-14) | `Bytes<P>` / `BytesPage`, tag `0x0B`, `rindex03` hard-break, `InterfaceDecl.schema_hash`, geometry audit, `select!` byte→page conversion, ranged `Read` | Gate 2/3 still open (`ListRange` cross-check, selection↔replay bind) — both now [`selection-unbound-from-execution`](../issues/selection-unbound-from-execution.md), **top priority**; no `pages!` sugar |
| [`recur-sequence-break`](./recur-sequence-break.md) | proposed 2026-08-13 | — | whole; blocked on `recur-progress-commitment` rev 2. Weakens `lazy-list-recur` S4 to a prefix/terminal split |
| [`trace-end-windows`](./trace-end-windows.md) | **implemented** (2026-09-16) | both ends of a trace are provable: the head slice clamps and declares its measured length, a window at trace index 0 asserts the genesis opening state in place of a margin it cannot have, the `First` exemption is gone (so a one-item window is provable and every window gains an item of margin), and the final window's trace roots are revealed in full so detection there stops reading the packed fingerprint. Closes [`fraud-window-trace-ends`](../issues/fraud-window-trace-ends.md) §2/§3 and answers its framing question: `window_size` stays **one** parameter. Format break — `TRACE_COMMITMENT_DOMAIN` v1 → v2, guest image ids move | end-to-end run against `raster-inference` not redone since the break; `revealed_tail_roots` costs `window_size * 32` bytes, unmeasured against real proofs; the `Next` chain still has no end-to-end coverage (pre-existing) |
| [`trace-leaf-field-binding`](./trace-leaf-field-binding.md) | **implemented** (2026-09-16) | **three soundness breaks**, each demonstrated before being fixed and each proof of concept inverted into a regression test: `exec_index` (read by nothing) and `sequence_id` (checked for two step kinds of five) reached the trace leaf unverified, so the *diverging* item of any window could be forged by changing a field nobody checked; the sequence-scope witness was the parent's `FnInput` bound to nothing, so the check compared a value against a value the same party chose. Also the prerequisite defect that hid them — `resolve_inputs_sources` panicked on every top-level step with a non-inline input, because five unit fixtures modelled a trace shape the recorder stopped producing. Closes the `input_sources_witnesses` map the host had always shipped and no guest code read | `input_sources_witnesses` is still cloned whole into every step (~`w²` entries per window) — narrowing it wrongly drops a witness an honest proof needs, so measure first |
| [`window-seed-reconstruction`](./window-seed-reconstruction.md) | **implemented** (2026-09-09) | the recorder retains its stack per step (`recur_progress_after`); `prove()` reconstructs the seed on the prefix walk it already makes for storage; `step_transitions` threads it under the existing first-step guard. Plus Uncertainty 3's diagnostic, which names a mid-loop open instead of letting it read as a commitment mismatch. Three corrections to the proposal, recorded in its §Implementation record — notably the accessor **cannot** be keyed by `CfsCoordinates` (a site's `Start` and `End` share the bare site coordinate and hold opposite stacks), so it is keyed by `exec_index` | — |
| [`carried-state-channel`](./carried-state-channel.md) | proposed 2026-08-07 — **enhancement** | — | deliberately deferred until a second component is ready |
| [`recur-state-chaining`](./recur-state-chaining.md) | **implemented** (2026-09-11) | `RecurProgressFrame.state_commitment` + `RecurStateTransition` on the replay journal, the call record and the step record; the fold rule with no permissive `if let Some`; `state_in` bound to the step's own recorded input and a tile's copy to its replay-proven one. Split out of `loop-carried-state` §4. Also fixes **two latent defects it uncovered**: the recur-sequence *site* recorded only its input while the CFS declares one source per argument, so **no recur sequence was provable at all** (unnoticed because the guest's `verify_step_record_inputs` runs only on the fraud path); and `FnCallRecord.recur_control` was never set by any producer — all 31 initializers were `None` and the recorder folded `Continue`. Trace-format hard break; `program_commitment` moves for every program | the terminal `state_out` of a recur *sequence* is unpinned (needs a state-only/state+output discriminator the CFS does not record); iteration 0's literal seed is not pinned to the program's value, which needs `InputSource::InlineLiteral`; no end-to-end fraud-proof window over a recur sequence; `raster-inference` acceptance unrun |
| [`trace-event-vocabulary`](./trace-event-vocabulary.md) | **implemented** (2026-08-13) | `RecurSequenceIterationStart`/`End`; the naming rule and vocabulary table on `TraceEvent` | — |
| [`chain-repeat`](./chain-repeat.md) | **implemented** (2026-08-27) | `[[chain.repeat]]` with an authorized trip count (literal or stage-produced); `[chain.input]` named + indexed externals; `ChainShape` in the chain commitment; `ChainFaultKind::Shape` **added beside `Link`** (not a restoration of the removed `Execution`). `raster-inference`'s 35 `prefill_prepare_aux` stages are now one block, expanding to the identical 74 stages. Chain-commitment format break — `spec_digest` moves every recorded digest, and **closes S1** (`chain_spec_commitment`), still recorded as open in `chain-fraud-proof` and `chain-io-commitment` | external (`{ input = ... }`) counts; `while` mode; collapsing `prefill_range`, which needs the §7 donor rewrite plus a two-block split |
| [`recur-deferred-finalize`](./recur-deferred-finalize.md) | **implemented** (2026-08-28) | opt-in `finalize = false` on `call_recur!`, so a draft can take a second writer; six drivers gain a `*_with_finish` form and a `*_open` wrapper; a hidden `__raster_recur_auth_open_<tile>` entry point. Default unchanged — omitting the flag closes the draft as before. Attestation does not move: draft witnesses already attach to `TileExec` steps, one iteration at a time | two open questions in §Open questions — whether the CFS should mark an open recur explicitly rather than implicitly by entry point, and whether `RecurTileEnd` should record the draft's post-root instead of `output: None` |
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
                             │                                  (impl)
                             └──► program-manifest ◄── also reorganizes the [chain]
                                    (proposed)             table chain-repeat extends
                                        also borrows the authorized-value rule from
                                        dynamic-index-selection (impl)

chain-fraud-proof (impl) ──► chain-io-commitment ◄──── window-seed-reconstruction
   reuses the window/slice        (proposed)   satisfied      (impl)
   binding; disagrees on one      ▲            — a terminal window opening mid-recur
   checkpoint field               │              verifies as of 2026-09-09, so
                                  │              recur-heavy stages are challengeable
   chain-stage-execution ─────────┘
     (partial) supplies the determinism fact and the --stage machinery;
     its §5 refusal to touch the authenticated path is lifted there

bounded-collections (phases 1-2 impl)
        │
        ├──► lazy-list-recur ◄──── recur-progress-commitment ──► recur-sequence-break
        │         │   ▲              (rev 2 impl) │                (proposed)
        │         │   │                           └──► window-seed-reconstruction
        │         │   │                                       (impl)
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
                               (impl)            shared frontier/seed mechanism

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
- ~~**`window-seed-reconstruction` → nothing; it unblocks mid-loop windows.**~~ **Satisfied
  2026-09-09.** A window opening inside a live loop now reconstructs its seed from the trace
  prefix, so the de-facto "refuse to open mid-loop" behaviour — the design
  `recur-progress-commitment` §Problem explicitly rejected, arrived at by an unfilled parameter
  rather than by choice — is gone.
- **`recur-sequence-break` → `recur-progress-commitment` rev 2.** Not merely ordered after it:
  the break bit rides on the `recur_control` trace field rev 2 introduces, and S4′ rewrites the
  `close_site` rule rev 2 implements. Landing it first would mean implementing both halves of
  that proposal anyway, in the wrong order.
- **`chain-fraud-proof` → `program-chain`, `program-identity`.** Already satisfied; noted
  because it is why `program-chain`'s `proposed` header must be stale.
- ~~**`chain-io-commitment` → `window-seed-reconstruction`.**~~ **Satisfied 2026-09-09.** Its
  challenge is a *terminal-window* receipt, and for a recur-heavy stage that window very often
  opens inside a live loop. That is what made the mid-loop gap soundness-adjacent rather than
  merely inconvenient — a claimer could pick such a program and be unchallengeable. §3's remaining
  blockers are the assumed settlement/DA/bonding infrastructure, which gate *use*, not design.

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
2. **`sequence-grammar-closure` phase 2** — independent of the above; `draft-provenance` argues
   one row of its classification table and can be taken with it or separately. A nested call
   macro in another call's arguments is now rejected at expansion
   (`raster-macros/src/lib.rs`, `reject_nested_call_macros`), which is an instance of this
   proposal's rule that arrived early via a different failure.
3. **`authoring-skill-and-tooling`'s second half** (`cargo raster check`) — independent, and it
   is the enforcement surface for rules the type system cannot express.
4. **`zkvm-dry-run`** — the only thing that decides "does this tile actually run in the zkVM"
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
- **`loop-carried-state`, `draft-provenance`** — not blocked, not scheduled. `loop-carried-state`'s
  §Migration step 1 is **done**, as [`recur-state-chaining`](./recur-state-chaining.md); what
  remains there is the by-reference `RecurSequenceStateRef` and `fixpoint`.
- **`chain-io-commitment`** — steps 1–3 (the journal's output value, the terminality pin, the
  host plumbing) are landable now and leave the current protocol working. The checkpoint
  narrowing at step 4 is the point of no return. Its one in-repo blocker,
  `window-seed-reconstruction`, landed 2026-09-09, so recur-heavy stages are now challengeable.
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
