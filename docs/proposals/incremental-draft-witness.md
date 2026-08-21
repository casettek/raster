# Proposal: `incremental-draft-witness` — witness a draft append with a frontier, not the whole log

Status: implemented 2026-08-15

Related:
- [`lazy-list-recur.md`](./lazy-list-recur.md) — **the mirror image, and the model for this
  document.** That proposal removed the eager whole-list resolve on the *read* side:
  `auth_ref_trace` materialized an entire list to obtain one `SelectionCommitment`, and was
  replaced by a 41-byte `0x0A` metadata payload proving `(len, elements_root)` without touching
  an element. This is the same disease on the *write* side — an entire list materialized to
  obtain one draft root — and the same cure.
- [`window-seed-reconstruction.md`](./window-seed-reconstruction.md) — its §5 already names
  `active_drafts` as a consumer. A frontier is exactly the seed a fraud-proof window opening
  mid-draft needs, so the two share a mechanism; §*Uncertainties* asks which one owns it.
- [`draft-provenance.md`](./draft-provenance.md) — the other open draft proposal. **Orthogonal,
  not an alternative**: it is about `finalize` severing provenance, this is about witness size.
  Neither blocks the other.
- [`carried-state-channel.md`](./carried-state-channel.md),
  [`loop-carried-state.md`](./loop-carried-state.md) — same carried-state family.
- [`bounded-collections.md`](./bounded-collections.md) — established that a collection is never
  passed whole across a tile boundary. A draft's *witness* is the remaining place where one
  still is.

Motivating program: `raster-inference`'s `prefill_finalize`, which appends 262,144 logits to one
draft and cannot finish.

## Problem

A draft is the sanctioned home for anything that grows. The authoring rule says so directly:
"each step appends its increment instead of re-materializing and re-committing the whole
object", and `output = new!(T)` is the recommended alternative to accumulating in `state`.

The **root** is incremental in that sense. The **witness is not.**

```rust
// raster-core/src/draft.rs:141
pub enum DraftFieldValue {
    Set(DraftValue),
    Append(Vec<DraftValue>),   // ← every element pushed so far
}

// raster-core/src/draft.rs:153
pub struct DraftStateWitness {
    pub schema: SchemaNode,
    pub fields: Vec<(String, DraftFieldValue)>,
}
```

Every tile step that touches a draft captures the full pre-state
(`raster-runtime/src/storage.rs:300`, `draft_state_witness`, which clones `state.fields`
wholesale), carries it on `FnCallRecord.draft_transition_witness`
(`raster-core/src/trace.rs:281`), and the transition guest rebuilds the root from it.

So a draft accumulating N elements costs **O(N) witness bytes and O(N) hashes per step**, and
**O(N²) overall**.

### Measured

`prefill_finalize` recurs over 8,192 weight pages, appending 32 logits per page. Sampling the
per-event sizes in its `trace.bin` (frame-length prefixes, no decode required):

| position in run | replay-unit size |
| --- | --- |
| 0% | 224 KB |
| 25% | 735 KB |
| 50% | 1.26 MB |
| 75% | 1.79 MB |
| 90% | 2.11 MB |
| 99% | 2.30 MB |

Linear, as predicted. The constant checks out too: at that point 2,628 pages × 32 = 84,096
entries had accumulated, so 2.30 MB / 84,096 ≈ **27 bytes per entry** — which is what a postcard
`DraftValue::Struct(vec![("token_id", U32), ("value", I32)])` weighs with its field-name strings
included. The model is understood, not merely observed.

Extrapolated to the full 8,192 pages:

- tail witness **7.1 MB**, for a step whose useful increment is 32 entries;
- total trace **29.0 GB**, for an output whose entire content is 2 MB.

The stage ran 90+ minutes without finishing. It is not slow; it is the wrong asymptote.

### The guest cost is the binding one

Host RSS and trace bytes are the visible symptom. The disqualifying number is cycles, because
`verify_draft_transition` runs **inside the zkVM**
(`raster-prover/guests/transition/src/checks/drafts.rs:13`), and it rebuilds the list root
twice per step:

```rust
verify_witness_root(&witness.pre_state, root_before)          // drafts.rs:55  — O(N) hashes
let (_, root_after) = apply_draft_ops(&witness.pre_state, ops) // drafts.rs:80  — O(N) hashes
```

Both route through `draft_root_from_witness` (`draft.rs:817`) →
`draft_tree_from_witness`, which *clones the whole accumulated list* into a `DraftValue::List`,
→ `list_root_from_hashes` (`draft.rs:566`), which hashes every element and every internal node.

Summed over the run that is on the order of **4.3 × 10⁹ SHA-256 invocations** to verify one
stage. There is no proving budget that absorbs this.

### What the witness is actually for

The whole obligation, read off `drafts.rs`, is narrow:

> supply a preimage of `root_before` that `ops` can be applied to, yielding `root_after`.

The guest never inspects the accumulated elements. It never needs their values. It needs only
enough information to recompute a Merkle root — and for an append-only list that is not the
list.

### The cost to authors, today

Because nothing documents this, the affordable size of a draft is discovered by running out of
time. In `raster-inference` it forced a routine boundary to be collapsed: `decode.select_token`
was fused into `prefill_finalize` and rewritten as a scalar argmax fold, purely to avoid
materialising a list the design wanted as a first-class artifact. That is a modelling decision
made for a witness-encoding reason, which is the tell that the encoding is wrong.

## Design

### 1. The witness carries a frontier, not a log

For an append-only list, appending changes only the right edge of the Merkle tree. The left
siblings along that edge — O(log N) digests — are sufficient to recompute the root.

In the **witness only**, replace the append log with an append frontier:

```rust
pub enum DraftFieldValue {
    Set(DraftValue),
    Append(Vec<DraftValue>),          // still the runtime's own representation
}

// what crosses into the trace / guest:
pub enum DraftWitnessField {
    Set(DraftValue),
    Append { len: u64, frontier: AppendFrontier },
}
```

`SerializableFrontier { position, leaf, ommers }` already exists at
`raster-core/src/transition.rs:26` and is exactly this shape — the storage trace tree uses it.
Reuse it, or mirror it under a draft-specific name if the padding rules are better kept apart
(§3).

### 2. `DraftOp` is unchanged

```rust
// raster-core/src/draft.rs:147 — untouched
pub enum DraftOp {
    Set { field: String, value: DraftValue },
    Push { field: String, value: DraftValue },
}
```

The pushed values already travel in the replay journal, which is per-step and O(1) in the
accumulated length. The guest takes new elements from `ops` and their leaf hashes from
`draft_value_payload_and_root` (`draft.rs:590`) — the same function `list_root_from_hashes`
consumes today. Nothing new needs to be recorded.

### 3. `list_root_from_hashes` must not change

This is the constraint that shapes everything else, and it should be read before any
implementation begins. `draft.rs:594` states the invariant:

> a draft's root must be indistinguishable from the selection-tree root of the same data (that
> equality is what lets a finalized draft be selected into like any other object)

Changing the hash would invalidate every committed artifact, every `Raster.lock`, and every
fixture in every dependent repository. **This proposal changes how a root is witnessed, never
what the root is.** The frontier must reproduce the existing function bit-for-bit, padding rule
included.

That is a real constraint rather than a formality, because `list_root_from_hashes` does not pad
with a zero element — it **duplicates the last node** at any odd level:

```rust
while level.len() > 1 {
    if level.len() % 2 == 1 {
        let last = level.last().cloned().unwrap();
        level.push(last);                       // duplicate-last
    }
    // pair up with H("list-node", l, r)
}
```

Duplication is nonetheless confined to the incomplete right edge, which is precisely what a
frontier holds. Worked cases:

```text
N=3   level 1: H(a,b)  H(c,c)          N=4   level 1: H(a,b)  H(c,d)
      level 2: H(H(a,b),H(c,c))              level 2: H(H(a,b),H(c,d))
```

`H(c,c)` becomes `H(c,d)`; `H(a,b)` is untouched. A frontier of `{ leaf: c, ommers[1]: H(a,b) }`
computes both.

```text
N=5   level 1: H(a,b) H(c,d) H(e,e)     N=6   level 1: H(a,b) H(c,d) H(e,f)
      level 2: H(..)  H(H(e,e),H(e,e))        level 2: H(..)  H(H(e,f),H(e,f))
```

`H(e,e)` becomes `H(e,f)` and everything above it recomputes, but
`ommers[2] = H(H(a,b),H(c,d))` is untouched. Same frontier, same result.

The general rule the implementation encodes: climbing from the last leaf at index `idx = len-1`,
at each level take `H(ommers[L], node)` when `idx` is odd and `H(node, node)` when it is even —
because an even index at that level *is* the last node, which is what the padding duplicates.

**This was verified computationally, not argued.** A reference implementation of both functions
agrees on the exact digest for N = 0..300 and at N = 4096. At N = 4096 the frontier is **424
bytes** against **110,592 bytes** of append log. That check should land as a test (§Verification).

### 4. A root path that composes field roots

`draft_root_from_witness` (`draft.rs:817`) currently builds a whole `DraftValue` tree and hashes
it. It needs a sibling that composes the struct root from per-field **roots**:

- set-once fields contribute the root of their (small) value, as today;
- append fields contribute `list_root` derived from `(len, frontier)` without touching elements;
- the struct root combines them through the existing `struct_commitments_root`, unchanged.

Plus `advance_append_frontier(frontier, leaves) -> (frontier', root)`, folding the pushed leaves
in at O(log N) each, used by `apply_draft_ops` to produce `root_after`.

### 5. Secondary, and deliberately not bundled

Every witness also carries a full `SchemaNode`, cloned and re-serialized on every step, though
the guest uses it only for `compute_schema_hash` and field-mode lookup. Next to the list this is
second-order, but it is the same class of mistake: per-step carriage of a per-draft constant. It
should be recorded as a follow-up — carry the schema once per draft chain, or carry only its
hash plus the field modes — and **not** folded into this change, whose soundness argument should
stay confined to the list root.

## Soundness

What the frontier proves: that `root_before` has a preimage consistent with a list of `len`
elements whose right edge is the given ommers, and that applying `ops` to it yields `root_after`.
That is the same statement the full log supports, because the root function is unchanged and the
frontier reproduces it exactly.

What it does not change:

- **Root chaining.** `active_drafts` still asserts `tracked_state.root == root_before`
  (`drafts.rs:75`), and `TrackedDraftState` (`draft.rs:165`) is untouched by the core proposal.
- **Set-once semantics.** `apply_draft_ops` keeps rejecting a second `Set` and a `Push` to a
  set-once field; those checks read the schema, not the elements.
- **Finalize.** Materializing the full value at finalize is correct and stays — it is O(N)
  **once**, which was never the problem. The runtime holds the real draft in memory regardless;
  only the *witness* representation changes.

What weakens: nothing that was load-bearing. The witness never authenticated element *values* to
the guest — it authenticated a root, and it still does.

## Modules touched

- `raster-core/src/draft.rs` — witness field type, frontier, root-from-roots path,
  `apply_draft_ops`, `verify_witness_root`. `list_root_from_hashes` **unchanged**.
- `raster-core/src/transition.rs` — reuse or mirror `SerializableFrontier`.
- `raster-runtime/src/storage.rs` — `draft_state_witness` emits a frontier instead of cloning
  the field log; the runtime keeps its own full `DraftRuntimeState` for finalize.
- `raster-prover/guests/transition/src/checks/drafts.rs` — unchanged in shape; both call sites
  keep their signatures.

## Phasing

1. **Types and root path** in `raster-core`, with the equivalence test against
   `list_root_from_hashes` as the gate. Nothing else lands until digests match.
2. **Recorder** — `storage.rs` emits frontiers.
3. **Guest** — consume the new field form.
4. **Migration.** The witness is trace-format-visible, so this is a **trace-compatibility
   break**: existing `trace.bin` and `commit.bin` artifacts do not decode against the new type.
   It costs nothing to re-run, but it must be stated, and it wants the same treatment
   `paged-bytes` gave the `rindex03` hard break.

   Landed as exactly that. What moves and what does not, stated precisely, because "no image-id
   change" would be wrong:

   - **Input fixtures do not move.** No parsing rule and no payload tag changed, so every
     `.rindex` / `.rastered` file and every input commitment is untouched.
   - **`program_commitment` does not move.** It is `sha256(domain ‖ program.bin)` over the
     `ProgramDefinition` frame — CFS, interface decls, `SchemaNode` hashes — none of which this
     change touches.
   - **Guest image ids do move.** The transition guest links `raster-core`, and `raster-core`
     changed; that is true of any change to this crate, and the transition guest's checks are
     where the saving is realized. Tile images may move for the same linkage reason.

   The repository pins no image ids — there is no `Raster.lock` here — so nothing in-tree
   breaks, but a dependent repository holding a lock file must re-lock.

   `examples/hello-tiles/fraud_commit.bin.fraud-proof` is **not** regenerated. It is a saved
   `risc0_zkvm::Receipt`, and a receipt's journal carries `TrackedDraftState` — untouched by
   this change — not the witness, so nothing in it fails to decode. It is stale for the
   image-id reason above, which it already was before this change.

## Verification

1. **Root equivalence (the gate).** Property test: for random element sequences N = 0..1024,
   the frontier root equals `list_root_from_hashes`. Include N=3,4,5,6 as explicit cases — they
   are the padding transitions.
2. **Draft roots do not move.** Finalized-draft roots for `examples/hello-tiles` must be
   byte-identical before and after. This is the invariant of §3; if it moves, stop.
3. **Witness size is flat.** Regression appending ~10⁵ elements asserting per-step witness size
   is O(log N) — i.e. bounded by a constant over the range — rather than growing.
4. **Round-trip.** `hello-tiles` commit/audit, and `cargo raster program --verify` unchanged
   (this touches no tile code, so image ids must not move).
5. **Acceptance.** Re-run `raster-inference`'s `prefill_finalize`. It should complete in
   minutes with a trace in the tens of MB.

## Performance

For `prefill_finalize` (N = 262,144 over 8,192 steps, ~27 B/entry):

| | per-step witness | total trace | guest hashes |
| --- | --- | --- | --- |
| append log (today) | 224 KB → **7.1 MB** | **29.0 GB** | ~4.3 × 10⁹ |
| frontier | ≤ 648 B (≤ 19 ommers) + 864 B ops | **12.4 MB** | ~1.0 × 10⁷ |

≈ **2,300× less trace**, and the per-step cost stops depending on how much has been appended —
which is the property the authoring rule already promises.

## Uncertainties for review

*Resolved at implementation; recorded here rather than deleted, because the reasons are the
reasons the code has the shape it has.*

1. **Who owns the frontier.** It can live in the witness (self-contained steps, simplest) or be
   reconstructed per `window-seed-reconstruction` and carried in `TrackedDraftState` (steps after
   the first need no append witness at all, but a window's first step needs a seed). The second
   is strictly smaller and strictly more coupled. This proposal assumes the first and defers.
   → **Resolved: the witness owns it.** `TrackedDraftState` is untouched, so
   `window-seed-reconstruction` is free to take the second route later without unwinding this.
2. **Whether `DraftFieldValue` should split.** Above it is shown as a separate
   `DraftWitnessField`, keeping the runtime's own representation intact. Reusing one enum with a
   new variant is less code and more room to construct a witness that is not a witness.
   → **Resolved: split.** `DraftWitnessField` is what crosses into the trace; `DraftFieldValue`
   stays the runtime's own representation and is what `finalize` materializes from. The extra
   enum buys the property that a witness *cannot* express an append log.
3. **`Map` fields.** Only `AppendOnlyVec` and `SetOnce` modes exist today
   (`SchemaFieldMode`), so nothing else accumulates. If a growing map mode is ever added it needs
   its own incremental story; this proposal does not generalise to one.
   → **Unchanged.** No map mode was added.

## Outstanding at implementation

- **§5 is not done, by design.** The witness still carries a full `SchemaNode` per step, and
  still carries a set-once field's whole `DraftValue` rather than its root. Both are per-draft
  constants paid per step — the same class of mistake as the list, second-order in size, and
  deliberately excluded so the soundness argument stayed confined to the list root. They are one
  follow-up, not two.
- **The acceptance run (§Verification 5) has not been re-run here.** `raster-inference` is a
  separate repository; the projections in §Performance stand until it reports.

## Implementation notes

Two things the design section did not anticipate:

1. **The host had the same disease.** `raster-runtime`'s `apply_draft_push` recomputed the whole
   draft root on every push, so a draft cost O(N) *host* hashes per push — larger than the guest
   cost this proposal was written about, and the likelier explanation for the 90 minutes. The
   runtime now keeps a live `AppendFrontier` per append field beside the values, and recomposes
   the root from per-field roots. §Modules touched understated this: it is not "emit a frontier
   instead of cloning the log", it is "maintain one".
2. **`AppendFrontier` lives in `input.rs`, not `draft.rs`.** It has to agree with
   `list_root_from_hashes` bit for bit, so it sits beside it — the same reasoning that already
   put `struct_commitments_root` there and had `draft.rs` reuse it rather than restate it.
   `transition::SerializableFrontier` was not reused: it is `std`-gated, `position`- rather than
   `len`-shaped, and belongs to a tree with a different combine rule.
