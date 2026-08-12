# Proposals — status and dependencies

Index of `docs/proposals/`. Last reviewed 2026-08-07.

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
| [`lazy-list-recur`](./lazy-list-recur.md) | proposed 2026-08-05 (rev 2) | — | whole; 6 phases, ship together |
| [`recur-progress-commitment`](./recur-progress-commitment.md) | proposed 2026-08-07 | — | ships **with** `lazy-list-recur` phase 5 |
| [`paged-bytes`](./paged-bytes.md) | proposed 2026-08-04 (rev 3) | — | blocked on `lazy-list-recur` |
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
        ├──► lazy-list-recur ◄──── recur-progress-commitment
        │         │   ▲                     (ships together, phase 5)
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

- **`paged-bytes` → `lazy-list-recur`.** Three gates, per `paged-bytes.md`: a `Bytes` region
  cannot be swept until §1–§4 land; a sweep cannot be called *complete* until §5 lands; and no
  sweep is end-to-end sound until the storage-selection-to-replay binding lands (`paged-bytes`
  §3.3), which is out of scope for both.
- **`lazy-list-recur` §5 → `recur-progress-commitment`.** §5 defines the per-iteration facts and
  the completeness rules; it does not define the carrier those rules accumulate in. A
  fraud-proof window verifies one step at a time and the prover chooses where windows open, so
  without a committed carrier the rules bind only in a window that happens to contain iteration
  0. One change, not two.
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

1. **`lazy-list-recur` + `recur-progress-commitment`** — one change, six phases, all shipping
   together. Every image id moves; `paged-bytes` unblocks behind it.
2. **`sequence-grammar-closure` phase 2** — independent of the above; `draft-provenance` argues
   one row of its classification table and can be taken with it or separately.
3. **`authoring-skill-and-tooling`'s second half** (`cargo raster check`) — independent, and it
   is the enforcement surface for rules the type system cannot express.

`trace-event-vocabulary` was on this list and landed 2026-08-13, ahead of the other three: it was
the cheapest item and had to precede any site-level recur record. `recur-progress-commitment`'s
`advance`/`close_site` split can now use those names.

## Blocked or deliberately deferred

- **`paged-bytes`** — blocked on `lazy-list-recur`.
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
