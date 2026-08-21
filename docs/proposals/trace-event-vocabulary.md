# Proposal: `trace-event-vocabulary` — an event should say which level it belongs to

Status: **implemented** (2026-08-13) — **enhancement and prerequisite.** It changes no commitment,
moves no image id, and landed on its own. It is listed as a prerequisite because the names it
frees are the ones a site-level record would want, and adding those first would put two meanings
one letter apart in the same `match`.

Landed as specified, with two deviations recorded here rather than silently taken:

- **No binary-trace compatibility test.** §Verification's headline check — a trace recorded
  before the rename decodes identically after — was dropped: there were no binary trace fixtures
  to extend, and backward compatibility with previously recorded traces was ruled out of scope by
  the author. §4's "do not reorder" rule is now carried by the doc comment alone, unenforced.
- **§Verification's "JSON golden files regenerated" was a no-op.** No golden JSON traces exist in
  the repo. The only JSON-visible surface is `publishers/json.rs:34`, whose own test asserts on
  `SequenceStart` — not a renamed variant.

Related:
- [`lazy-list-recur.md`](./lazy-list-recur.md) — §5's recur-sequence rules S2–S4 are read from
  "iteration boundary events". Which events those are is currently a matter of knowing the
  codebase rather than reading the name.
- [`recur-progress-commitment.md`](./recur-progress-commitment.md) — its `advance`/`close_site`
  split is exactly the iteration/site distinction this proposal makes legible.

## Problem

`TraceEvent` (`raster-core/src/trace.rs:615-628`) mixes two levels — the CFS **item** (a recur
site) and one **iteration** of that item — and only half of it says which is which:

```rust
SequenceStart(FnCallRecord),
SequenceEnd(FnCallRecord),
RecurSequenceStart(FnCallRecord),        // per ITERATION, named as if per site
RecurSequenceEnd(FnCallRecord),          // per ITERATION, named as if per site

TileExec(FnCallRecord),
RecurTileIterationExec(FnCallRecord),    // per ITERATION, named so
RecurTileExec(FnCallRecord),             // the SITE
RecurSequenceExec(FnCallRecord),         // the SITE
```

The tile family distinguishes the two by name; the sequence family does not. This is an
inconsistency inside one enum, not a matter of taste, and the misreading it produces is not
hypothetical — `RecurSequenceStart` reads as "the recur sequence begins" and denotes "iteration
*i* of the recur sequence begins".

Three confirmations that the sequence pair is per-iteration:

- **who publishes them** — `gen_recur_sequence_step_function` (`raster-macros/src/recur.rs:769`,
  `:781`), the step function, once per iteration;
- **where they land** — `site ++ [i]`, recorded as `StepKind::SequenceStart` / `SequenceEnd`
  (`recorder.rs:602-610`, `:626-632`);
- **what they carry** — the iteration's `FnInput`, holding the
  `RecurSequenceInputTraceMarker { index, len, item }` (`raster/src/input.rs:131-137`). The
  site's own binding is carried by `RecurSequenceExec`.

### The second inconsistency: two mechanisms for the same marking

A recur **sequence** iteration is marked *explicitly* — the step function publishes
`RecurSequenceStart`/`End` by name. A recur **tile** iteration is marked *implicitly* — the tile
wrapper publishes an ordinary `TileExec`, and `publish_trace_event` rewrites it to
`RecurTileIterationExec` when a thread-local depth counter is non-zero
(`raster-runtime/src/tracing.rs:144-157`):

```rust
let event = RECUR_TRACE_DEPTH.with(|depth| {
    if depth.get() > 0 {
        match event {
            TraceEvent::TileExec(record) => TraceEvent::RecurTileIterationExec(record),
            other => other,
        }
    } else { event }
});
```

So the classification is positional, not structural: *any* tile executing while a recur-tile
site is on the stack is reclassified. That is correct today — only iterations run under
`RecurTraceScopeGuard`, and the recorder asserts the invariant ("Ordinary tile execution cannot
occur while recur iterations are active", `recorder.rs:650`) — but the rule lives in a
thread-local and an assert rather than in the vocabulary.

## Design

### 1. The naming rule

> An event is named for the **CFS item** it belongs to. If it denotes an *iteration* of that
> item rather than the item itself, the name says `Iteration`. Unmarked means the item.

This is the rule the tile family already follows; it is stated here so the sequence family can
follow it too, and so a later addition cannot quietly break it.

### 2. The renames

| today | after | denotes |
| --- | --- | --- |
| `RecurSequenceStart` | **`RecurSequenceIterationStart`** | iteration *i* opens |
| `RecurSequenceEnd` | **`RecurSequenceIterationEnd`** | iteration *i* closes |

Nothing else is renamed. `RecurTileIterationExec` already complies; `RecurTileExec` and
`RecurSequenceExec` are unmarked and denote their items, which the rule makes correct rather
than accidental.

### 3. The vocabulary table, in the enum's doc comment

The artifact that would have prevented the confusion, kept where the definition is:

| event | level | published by | becomes | at coordinates |
| --- | --- | --- | --- | --- |
| `ProgramStart` / `ProgramEnd` | program | entrypoint codegen | `StepKind::ProgramStart` / `ProgramEnd` | `[]` |
| `SequenceStart` / `SequenceEnd` | item | `#[sequence]` wrapper | `StepKind::SequenceStart` / `SequenceEnd` | `[s]` |
| `TileExec` | item | `#[tile]` wrapper | `Exec(Tile)` | `[s]` |
| `RecurTileIterationExec` | iteration | `#[tile]` wrapper, **reclassified at publish** | `Exec(Tile)` | `[s][i]` |
| `RecurSequenceIterationStart` / `…End` | iteration | recur-sequence step fn | `StepKind::SequenceStart` / `SequenceEnd` | `[s][i]` |
| `RecurTileExec` | item | recur-tile driver, **after the loop** | `Exec(RecurTile)` | `[s]` |
| `RecurSequenceExec` | item | recur-sequence driver, **after the loop** | `Exec(RecurSequence)` | `[s]` |

Two facts in that table are worth stating in prose because they surprise every reader: a recur
site's own event is emitted **last**, after every iteration it contains; and a recur tile's
iterations are recorded as `ExecTarget::Tile`, never `RecurTile` — `RecurTile` names the site
only.

### 4. Regroup with comments; do **not** reorder

Postcard encodes an enum variant as the varint of its **declaration index**, with no name. So
reordering variants silently changes the meaning of every previously recorded binary trace — an
old `TileExec` would decode as whatever now sits at that index. Reordering is therefore not a
cosmetic operation and must not be done as part of a tidy-up.

The grouping stays exactly as it is, and the levels are made visible with section comments and
the table above. If a reorder is ever wanted, it needs a trace-format version tag and a decoder
that refuses older ones — a separate change with a real migration, not a rename.

### 5. Leave the implicit reclassification alone, but document it

Making the tile side explicit (publishing `RecurTileIterationExec` directly from the driver's
closure) would remove the thread-local, but it means the `#[tile]` wrapper needs to know it is
being invoked as a recur step — which is exactly the coupling the depth counter exists to avoid.
Keep the mechanism; document it in the table (the "reclassified at publish" note) and keep the
recorder's assert as its backstop. Recorded as §Uncertainty 2 rather than silently settled.

## What this deliberately does not do

- **No site-level `RecurSequenceStart`/`End`.** Freeing those names is the point of the rename,
  but adding the records is a trace-format change with its own design (the closing record keeps
  the storage write; `SequenceEnd` carries no `StorageRoots`). That belongs in its own proposal.
- **No change to `StepKind`, `ExecTarget`, or any commitment.**
- **No change to what any event carries.**

## Why it is free

| artifact | effect |
| --- | --- |
| binary traces (postcard) | **unchanged** — variants encode by index, and the order does not move |
| `StepRecord`, `hash_trace_item`, fingerprints | **unchanged** — `StepKind` is untouched |
| image ids, `commit.bin`, stage checkpoints | **unchanged** |
| JSON traces | changed — `serde_json::to_string(&event)` (`publishers/json.rs:34`) is externally tagged, so variant names appear |

This is what lets it land independently of the `lazy-list-recur` migration rather than waiting
for it.

## Modules touched

| file | change |
| --- | --- |
| `raster-core/src/trace.rs` | the two renames; the vocabulary table and naming rule as doc comment; a note that variant order is load-bearing |
| `raster-macros/src/recur.rs` | 2 producer sites (`:769`, `:781`) |
| `raster-runtime/src/tracing/recorder.rs` | 8 match arms |
| `crates/raster/tests/{recur_draft,external_selection}.rs` | 4 assertions |

14 references, all in-crate. No consumer outside `crates/`.

## Verification

- **The free-ness property, tested directly:** a binary trace recorded before the rename decodes
  identically after it. If this fails, a variant moved and the change is not what it claims.
- JSON golden files regenerated; the diff must contain only the two names.
- One test per event asserting the vocabulary table is true of the recorder: which `StepKind` it
  becomes and the shape of its coordinates (`[s]` vs `[s][i]`). The table is the deliverable, so
  it should be executable rather than prose.
- A recur-sequence trace and a recur-tile trace both still record their iterations under the
  site and their site record last — the existing suites cover this; they must pass untouched
  apart from the renamed identifiers.

## Uncertainties for review

1. **Should the site be marked explicitly too** — `RecurTileSiteExec` / `RecurSequenceSiteExec`?
   Recommendation: no. "Unmarked = the item" matches `TileExec` and `SequenceStart`, where the
   unmarked form is already the item, and marking both sides churns four more names for symmetry
   that the rule already supplies.
2. **Should the depth-based reclassification become explicit** (§5)? It would remove a
   thread-local from the publish path at the cost of coupling the `#[tile]` wrapper to its
   caller. Not decided here.
3. **JSON trace consumers outside this repository.** The rename is visible to anything reading
   `trace.json`. Nothing in-repo reads it, but tooling elsewhere might.
