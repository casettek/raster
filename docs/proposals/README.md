# Proposals — status and dependencies

Index of `docs/proposals/`. Last reviewed 2026-08-14.

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
| [`draft-provenance`](./draft-provenance.md) | proposed (2026-07-30) | — | whole |
| [`loop-carried-state`](./loop-carried-state.md) | proposed (2026-07-30) | — | whole; see the note under *Dependencies* |
| [`lazy-list-recur`](./lazy-list-recur.md) | **phases 1–6 implemented** (2026-08-13/14) | metadata payload, `ListCursor`, driver-level chunking + range descent, per-item bindings, §5 journal facts, rules 1–7 + S1/S3/S4 enforced | **rule 8 unimplemented** (no `ListRange` cross-check) and S2 unenforced; the peak-RSS acceptance benchmark was never run — see §Outstanding at implementation |
| [`recur-progress-commitment`](./recur-progress-commitment.md) | **rev 2 implemented** (2026-08-14) | `recur_progress.rs`, the trace `recur_control` bit, site `Start`/`End` events, recorder stamping, guest advance-and-compare — recorder and guest agree on every commitment | mid-loop window seeds, split out as `window-seed-reconstruction` |
| [`paged-bytes`](./paged-bytes.md) | **rev 3 implemented** (2026-08-14) | `Bytes<P>` / `BytesPage`, tag `0x0B`, `rindex03` hard-break, `InterfaceDecl.schema_hash`, geometry audit, `select!` byte→page conversion, ranged `Read` | Gate 2/3 still open (`ListRange` cross-check, selection↔replay bind); no `pages!` sugar |
| [`recur-sequence-break`](./recur-sequence-break.md) | proposed 2026-08-13 | — | whole; blocked on `recur-progress-commitment` rev 2. Weakens `lazy-list-recur` S4 to a prefix/terminal split |
| [`window-seed-reconstruction`](./window-seed-reconstruction.md) | proposed 2026-08-14 | — | whole; small. Without it a fraud-proof window opening mid-loop is rejected — the design `recur-progress-commitment` explicitly refused, reinstated by an unfilled parameter |
| [`carried-state-channel`](./carried-state-channel.md) | proposed 2026-08-07 — **enhancement** | — | deliberately deferred until a second component is ready |
| [`trace-event-vocabulary`](./trace-event-vocabulary.md) | **implemented** (2026-08-13) | `RecurSequenceIterationStart`/`End`; the naming rule and vocabulary table on `TraceEvent` | — |

## Dependencies

```
program-start ──┐
program-end ────┼──► program-identity ──► program-chain ──► chain-fraud-proof
                │         (impl)             (partial)          (impl)
                └── (impl)

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

### The non-blocking edges, stated

- **`loop-carried-state` → `carried-state-channel`.** `loop-carried-state` §2 proposes a
  `TrackedStateRoot` map "mirroring `active_drafts`", which would reproduce that map's
  window-open gap. It should extend the channel instead. Neither proposal blocks the other; the
  ordering only decides whether the trace format breaks once or twice.
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
