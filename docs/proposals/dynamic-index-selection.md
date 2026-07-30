# Proposal: `dynamic-index-selection` — selecting a list element by an authorized value

Status: Phase 1 + Phase 2 implemented (2026-07-30; proposed 2026-07-29).
Phase 3 (migrating `raster-chain-inference`) is not done — it lives in a
separate repository and still carries the unsettled width-check decision the
appendix describes.

Two things the implementation settled that this document had left open or wrong:

- **`RecurSequenceInput::into_ref()`** is the "small addition to the
  recur-sequence input surface" the appendix anticipated. It is inherent, not a
  trait method, because the blanket `impl<T: Serialize> IntoAuthRef<T> for T`
  makes `into_auth_ref()` ambiguous on a handle.
- **`IndexSource::resolve_index` takes `&self`.** Consuming the reference makes
  the shared-index case (§"The same index used twice") unwritable, because the
  `.clone()` it would require is a *computed* index by this grammar and the
  macro rejects it.
- **§5's claim that identity is "strictly about identity, not verification" is
  too weak.** §3 alone leaves *source substitution* open: nothing there stops a
  prover repointing a `BoundIndex` at a different authorized binding on the same
  step and adjusting the index to match. Phase 2 is what closes it, by pairing
  the declared index count against the recorded one. (The same gap exists today
  for literal indexes — selector paths are not part of program identity — so
  Phase 1 ships at exactly the bar literal indexes already meet, and Phase 2
  raises both.)
Related: [`bounded-collections.md`](./bounded-collections.md) — that proposal made
unbounded *materialization* unrepresentable; this one removes the reason programs
currently materialize collections they only need one element of.
Motivating program: `raster-examples/raster-chain-inference`, a Gemma prefill
pipeline expanded into an 11-stage chain.

## Problem

A `select!` index must be an integer **literal**:

```rust
let row = select!(EmbeddingRow, table.rows[7]);         // fine
let row = select!(EmbeddingRow, table.rows[token_id]);  // rejected by the macro
```

`split_selector_expr` (`crates/raster-macros/src/lib.rs:2717`) matches
`Expr::Index`'s index against `Expr::Lit` and `Expr::Range` and panics on
anything else: `"select! only supports integer literal indexes or `start..end`
ranges"`.

So a program that needs "the row for *this* token id" cannot ask for it. There
is no map type either. The only expressible formulation is a **scan**: iterate
the whole collection with `call_recur!` and compare a key field per element.
Each replay unit stays bounded — `bounded-collections` guarantees that — but the
pass costs one iteration per element, per lookup.

That is not a marginal inefficiency. In the Gemma chain, stage 2
(`input-embedding`) gathers one embedding row per prompt token by scanning the
embedding table at `chunk = 4`. For a 12-token prompt:

| | `tiny-gemma-dev`: vocab 280, hidden 4 | Gemma E4B: vocab 262,144, hidden 2048 |
| --- | --- | --- |
| replay units (`⌈vocab/4⌉ × 12`) | 840 | **786,432** |
| row bytes materialized | ~120 KB | **~48 GiB** |
| fold state re-committed | ~26 KB | **~12 GiB** |

Row bytes are `4 rows × hidden × 8` hex chars per unit; the fold state is
`RowMatch`, which carries the matched row and is re-committed every iteration.
The second column is not runnable, and it is the *cheap* stage: `prefill-
prepare-aux` runs the same shape against a per-layer PLE table, once per layer
(4 instances for `tiny-gemma-dev`, `num_hidden_layers` in general), and stage 1's
merge-rank lookup scans the tokenizer's merge table per piece pair.

The workaround programs reach for is worse than the scan: hoist the lookup out
of the authorized chain and let a host hand the gathered rows in as a committed
external. That trades an unaffordable proof for no proof at all — the binding
between "token id" and "the row that was used" stops being verified.

## What already exists

Three pieces of the mechanism are in the tree today.

**Runtime-valued index selection is implemented and proven.** The recur driver
builds an index segment from a loop counter, not a literal
(`ResolvedRecurList::select_item`, `crates/raster/src/input.rs:1096`):

```rust
let relative_selector = selector_path(Vec::from([SelectorSegment::Index(index)]));
```

Every recur iteration is therefore a dynamic-index selection already. Nothing
about the proof, the witness, or the guest is literal-specific.

**The inclusion proof exists.** `SelectionProofStep::List { index, len, siblings }`
with `ListProofSibling`/`ListProofDirection`
(`crates/raster-core/src/input.rs:145`) proves an element at an index against
the list root, and `fold_list_proof` verifies it. `SelectionProof` carries its
own `path`, so the claimed selector is witness data, not program data.
(`ListRange` is the sibling shape for `[a..b]` slices; this proposal leaves it
untouched.)

**The path/proof agreement check exists.** `step_proves_segment`
(`crates/raster-core/src/input.rs:501`) rejects a proof that proves one index
while claiming another:

```rust
(SelectionProofStep::List { index, .. }, SelectorSegment::Index(claimed)) => index == claimed
```

So the question this proposal answers is **not** "can we prove an element at a
runtime index" — that ships. It is: *where may the index come from, and what
binds it?*

## Why the recur case is sound and a data index is not

A recur iteration's index is the loop counter. Its provenance is structural: the
CFS records a `RecurTileItem` over a committed list, the trace records iteration
`i` of `n`, and the guest re-derives the same sequence of indexes from the same
committed source. The index is never *claimed* — it is generated by the driver
the schema pins.

An index taken from program data has no such provenance. If a sequence could
write `table.rows[token_id]`, the witness would carry a proof of *some* element
at *some* index, path-consistent and root-consistent — and nothing would say
that index was `token_id`. A prover free to choose the index is free to choose
the row: exactly the substitution the authorization chain exists to prevent.

**The missing piece is one binding, not new cryptography.**

## Goal

Let a selection's index come from an authorized reference, and make the witness
prove it did:

```rust
let token_id = select!(u32, prompt.token_ids[0]);           // an authorized u32
let row      = select!(EmbeddingRow, table.rows[token_id]);  // proposed
```

with the guarantee: *the element proved is the element at the index that the
named authorized value commits to*. A prover that substitutes a different row
must also substitute the index, which must then disagree with a value that is
itself committed.

Cost: the scan collapses to two inclusion proofs — one for the index value, one
for the element — i.e. O(log n) hashes instead of n materializations.

## Surface

`select!` accepts an index expression that is a **binding in scope**, and only
that (no arithmetic, no literals mixed in — the grammar rules of SKILL.md §4
apply unchanged):

```rust
select!(EmbeddingRow, table.rows[token_id])       // index from a binding
select!(EmbeddingRow, table.rows[7])              // literal, unchanged
select!(Block<Row>,   table.rows[a..b])           // ranges stay literal (below)
```

Type rule: the index binding must be an `AuthRef<T>` with `T` an unsigned
integer (`u8`/`u16`/`u32`/`u64`). A trait — `IndexSource` — carries that, so a
non-integer or non-authorized index is a compile error, not a runtime one.
`IndexSource` also surfaces the width, which §3 needs.

Ranges keep literal bounds in v1. A dynamic range multiplies the questions
(bound arithmetic, overlap with `chunk`, slice-width proofs) without changing
what the motivating programs need, which is a single-element gather.

## Mechanism

The design principle: **do not introduce a second way to authorize a value.**
A dynamic index is an authorized value the step already reads; it should arrive
through the path the guest already verifies, and the segment should only *name*
it.

### 1. A new selector segment

```rust
pub enum SelectorSegment {
    Field(String),
    Index(u64),
    Range { start: u64, end: u64 },
    /// Index taken from an authorized value. `index` is the value used on this
    /// run; `source` names the storage binding on this same step that supplied
    /// it, and `width` fixes how that value is encoded.
    BoundIndex { index: u64, source: StorageBindingName, width: IndexWidth },
}
```

`index` is per-run data, as it already is for recur. `source` is the provenance:
a key into the step's own `FnInput.storage` map
(`StorageInput = BTreeMap<StorageBindingName, StorageData>`,
`crates/raster-core/src/trace.rs:31`).

**This is the substantive change from the first draft**, which had the segment
carry `source: StorageRef, selector: SelectorPath` — the index's full
coordinates inline. That form makes the guest responsible for establishing, from
scratch, that the named ref is a value this execution authorized: it would have
to re-derive a store membership proof outside the read loop that already does
exactly that, and `SelectorPath` would become recursive (a path containing
paths). Naming a sibling binding instead makes the reference *closed* — it can
only point at something the step already proved — and keeps `SelectorPath` flat,
which matters because `verify_selection_witness` compares whole paths for
equality (`input.rs:739`).

### 2. The witness is two storage bindings, not a new witness shape

A step whose selection uses `BoundIndex` records **two** entries in
`FnInput.storage`:

```text
  "row"        StorageData { coordinates, commitment, selector: [.rows, BoundIndex{..}], selection }
  "row.index"  StorageData { coordinates, commitment, selector: [.token_ids, Index(0)],  selection }
```

Both are ordinary `StorageData`. The guest's existing read loop
(`crates/raster-prover/guests/transition/src/checks/store.rs:167`) already, for
every entry: finds the matching `StorageReadWitness`, runs
`verify_storage_read_witness` (append-log path + coordinate-index membership
against the current roots), asserts `commitment == selection.source_root_hash`,
and runs `verify_selection_witness`. Nothing in that loop needs to change.

Consequence worth stating plainly: the index binding is present in
`FnInput.storage` but has no corresponding entry in `FnInput.values`/`args` —
the tile does not take the index as a parameter. Today those two are in
lockstep. The recording side (`auth_ref_trace`, `crates/raster/src/input.rs:1396`)
must resolve the index `AuthRef` as a side binding when it resolves the element's,
and the guest must stop assuming `storage` and `values` are parallel.

### 3. Verifier obligations (fail-closed)

`step_proves_segment` gains one arm; the read loop gains one cross-check.

- **The element proof pins the index.** `(SelectionProofStep::List { index, .. },
  SelectorSegment::BoundIndex { index: claimed, .. }) => index == claimed`.
  Identical to today's `Index` rule — the proof must prove the index the segment
  claims.
- **The index binding commits to that same index.** For each `BoundIndex`
  segment in a binding's selector, look up `source` in the same step's storage
  map (**absent ⇒ reject**), then check

  ```text
  index_binding.selection.selected_hash == selection_payload_hash(encode_leaf(index, width))
  ```

  That is *encode-and-compare*, not decode-and-compare: the guest re-derives the
  canonical leaf bytes for the claimed `index` at the declared `width` and
  compares one hash against a value it has already verified. No integer decoder
  in the guest, no ambiguity about postcard's varint widths, and a value that
  does not fit `width` simply fails to match. If the recorded `index` exceeds
  what `width` can hold, the encoding differs and the check fails.
- **No self-reference.** `source` must not name the binding it appears in, and
  the `source` graph across a step's bindings must be acyclic. A one-pass check
  over a `BTreeMap` — cheap, and without it a binding could bootstrap its own
  index.
- **Authorization comes for free.** The index binding's root is authorized by
  the same `verify_storage_read_witness` call every other binding gets. There is
  no new provenance rule.

Nothing else changes: the transition guest folds the same way, and the fraud
guest inherits it, because a `BoundIndex` step is verified from witness data
exactly like a `List` step.

### 4. Out-of-range indexes

The first draft called an out-of-range index "a terminal program error, not a
proof failure." That is the right *intent* and the wrong mechanism, and it is
worth being precise, because the current code already decides the question:
`fold_list_proof` returns `false` when `len == 0 || index >= len`
(`crates/raster-core/src/input.rs:706`). Out-of-range is not merely un-attested
today — it is **unprovable**. There is no non-membership shape for a list node,
so a prover cannot produce a witness that says "there is no element 900 here."

Two coherent answers; v1 takes the first.

**v1 — the run aborts, unattested.** A `token_id` past the vocabulary means the
host cannot resolve the selection, so no trace is recorded and no output is
published. The protocol attests success only, so an aborted run is
indistinguishable from a run that never happened. This is fail-closed, costs
nothing, and is what a program *should* do about a token id outside its
vocabulary. Its limit is real: the program cannot *handle* the case, so it
cannot report which token was bad.

**Later — make length provable, so the program can gate.** The list root already
commits `len`: `selection_hash([b"list-root", len.to_le_bytes(), subtree])`
(`input.rs:284`), with the empty list as
`selection_hash([b"list-root", 0u64, b"empty"])` (`input.rs:263`). A step shape
that presents the element-subtree root directly —
`ListLen { len, elements_root }`, zero siblings — recombines to the list root and
therefore *proves* a length against an authorized commitment. Given that, a
program can `select!` the length, gate on it in a fallible tile, and fail with a
message it chose. That is a small, self-contained addition, and it is the
prerequisite for a handled out-of-range case rather than a bolt-on to it. It is
deliberately not in v1: the motivating programs need the gather, not the
diagnostic.

What v1 must *not* do is silently select something else, and it does not — the
fold rejects.

### 5. CFS treatment

The CFS records binding *kinds*, not selector paths (`InputBinding`,
`crates/raster-core/src/cfs.rs:614`), so a dynamic index does not change the
schema's topology. It should still change the program's *identity*: "reads the
element named by binding X" and "reads element 7" are different programs, and
under `program-identity` that difference must be visible in the committed
schema.

`InputBinding` is the right place: a new variant recording that a binding's
index is data-sourced and *which* item supplied it — the same information
`PriorItemOutput { intra_sequence_item_index }` already carries for values.
Note this is strictly about identity, not verification: §3's soundness argument
runs entirely on the step's own storage map and holds whether or not the CFS
records anything. Which is why identity is Phase 2 and not Phase 1.

## What this does not fix

- **The access pattern becomes data-dependent.** Today a program's reads are a
  function of its shape; with dynamic indexes they are a function of its inputs.
  The trace shape stays constant (one selection step, whatever the index), so
  replay and fraud proofs are unaffected — but anything that treated "which
  bytes were read" as static must stop doing so. Nothing in the current
  verifier does.
- **Confidentiality.** The index appears in the witness in the clear, and now
  also as a separately committed storage read. A program whose *index* is the
  secret gains nothing here, and arguably leaks slightly more than a scan did.
- **Whole-matrix materialization.** The other wall in the motivating chain —
  a layer's weights arriving as one tile argument — is a decomposition problem
  in program code, not a protocol gap. Dynamic indexes do not address it.
- **Maps.** Key→value lookup by hash still needs a `Map<K, V>` node; this
  proposal only makes *positional* lookup addressable. A vocabulary keyed by
  token id is positional, which is why it is the motivating case.

## Edge cases

- **Empty list**: `fold_list_proof` rejects `len == 0` before any sibling is
  consumed, so v1 aborts the run (§4). Note this differs from empty recur
  inputs, which skip: a *selection* has nothing to skip.
- **Index width**: fixed by the segment's `width`, which `IndexSource` derives
  from the binding's type. A signed index is a compile error.
- **Nested dynamic indexes** (`a.rows[i].cells[j]`): each `BoundIndex` segment
  names its own source binding, and the folding verifier already walks segments
  independently, so nesting costs one extra sibling binding per level and no new
  logic. The acyclicity check in §3 covers the whole step at once.
- **The same index used twice** (`a.rows[i]` and `b.rows[i]`): both segments name
  the same `source`, which resolves to one storage binding read once. Reusing
  the name is the point — it is what makes "the same index" mean the same index.
- **Dynamic index into a draft-built list**: fine — the list root is the draft's
  finalized root, which is already authorized.
- **`chunk` interplay**: none. Chunking pins a literal window size in the CFS;
  a dynamic index selects one element outside any recur.

## Migration

1. **Phase 1 — the segment and its proof.** `BoundIndex`, the
   `step_proves_segment` arm, the encode-and-compare cross-check and acyclicity
   check in the guest read loop, the `IndexSource` trait, the `select!`
   lowering, and the recording-side change in §2 that decouples `storage` from
   `values`. Existing programs are unaffected: literal indexes keep emitting
   `Index`. Ship with UI tests for the rejected forms (non-integer index,
   computed index, inline index, signed index) and negative guest tests for each
   §3 obligation — substituted index, absent source binding, self-referential
   source, index that overflows its declared width.
2. **Phase 2 — identity.** The `InputBinding` variant, so program identity
   distinguishes a data-sourced index from a literal one.
3. **Phase 3 — retire the scans.** In `raster-chain-inference`, `input-embedding`
   and `prefill-prepare-aux` replace their `scan_*` recurs with a single select
   (`input-embedding` worked out below); `RowMatch` and `PleEmbeddingMatch` — the
   largest non-attention recur states in that repo, each carrying a matched row
   through a fold — disappear with them. The appendix's second note flags one
   check that does *not* collapse mechanically and needs a decision per program.

## Appendix — the change to a real program

`raster-chain-inference`'s `input-embedding` gathers one embedding row per
prompt token. This is the pass whose numbers open this proposal.

### Today

`input-embedding/src/main.rs:20`:

```rust
#[sequence(kind = recur)]
fn embed_prompt_token(
    input: RecurSequenceInput<u32>,
    output: RecurSequenceOutput<ActivationSequence>,
    rows: List<EmbeddingRow>,
    hidden_size: u32,
) -> RecurSequenceOutput<ActivationSequence> {
    let query = call!(begin_row_lookup, input);          // materialize the id once

    let hit = call_recur!(                               // walk the WHOLE table
        tile = scan_embedding_rows,
        input = rows,
        chunk = 4,
        state = RowMatch { found: false, values_hex: String::new() },
        args = (query.clone(), hidden_size)
    );

    call!(append_activation_row, output, query, hit)
}
```

The scan tile compares a key field per element and carries the winning row in
fold state, which is re-committed on every chunk (`input-embedding/src/lib.rs:42`):

```rust
#[tile(kind = recur)]
pub fn scan_embedding_rows(
    input: RecurInput<Block<EmbeddingRow>>,
    state: RecurState<RowMatch>,
    query: RowQuery,
    hidden_size: u32,
) -> RecurState<RowMatch> {
    let mut state = state;
    let expected_len = (hidden_size * HEX_CHARS_PER_VALUE) as usize;
    for row in input.into_value() {
        let usable = row.token_id == query.token_id && row.values_hex.len() == expected_len;
        if !state.found && usable {
            state.found = true;
            state.values_hex = row.values_hex;   // the whole row, per chunk
        }
    }
    state
}
```

### With `BoundIndex`

The token id is the recur-sequence *item*, so it is already an authorized
reference — exactly what `IndexSource` wants. It goes straight into the selector
instead of through `begin_row_lookup`:

```rust
#[sequence(kind = recur)]
fn embed_prompt_token(
    input: RecurSequenceInput<u32>,
    output: RecurSequenceOutput<ActivationSequence>,
    rows: List<EmbeddingRow>,
    hidden_size: u32,
) -> RecurSequenceOutput<ActivationSequence> {
    let token_id = input.into_ref();                     // AuthRef<u32>, unmaterialized
    let row = select!(EmbeddingRow, rows[token_id]);      // one authenticated read
    call!(append_activation_row, output, row, hidden_size)
}
```

(`hidden_size` is retained here — see the second note below.)

Two details the first draft glossed, one of them awkward:

- `input` is a `RecurSequenceInput<u32>` handle, not an `AuthRef<u32>`. Getting a
  reference out of it without materializing it is a small addition to the
  recur-sequence input surface, and it is what keeps the id from needing its own
  tile call. (Falling back to `call!(begin_row_lookup, …)` also works and costs
  one tile per token, as today.)
- **`hidden_size` does not cleanly go away, and neither does the error list.**
  The current program rejects a table row whose packed width disagrees with the
  declared `hidden_size`. That check cannot move into `append_activation_row` as
  a failure, because `append_activation_row` runs inside a recur *sequence* and
  a recur sequence has no fallible form — which is exactly why the program
  accumulates `errors` and folds them in `main` today. So the width check has
  two honest homes, and the choice is a program-design question this proposal
  does not settle:

  1. **Keep it.** `errors`, `summarise_errors` and `assert_all_tokens_embedded`
     survive verbatim, now carrying only width violations rather than width
     violations *and* misses. The scan and `RowMatch` still go away; the
     post-hoc audit does not.
  2. **Drop it.** The embedding table is a committed external produced by
     `model-import`; "every row is `hidden_size` wide" is that importer's
     invariant, attested by the table's commitment, not something each program
     should re-derive per lookup. Under this reading the check was compensating
     for the scan's need to distinguish "wrong row" from "no row" — a distinction
     a proven index does not have.

  Option 2 is the cleaner end state and the one Phase 3 should aim at, but it
  moves a check across a trust boundary and should be argued on its own terms,
  not smuggled in as a side effect of dynamic indexes.

### What goes away

| removed | why |
| --- | --- |
| `scan_embedding_rows` | no scan |
| `RowMatch` | nothing to carry a match through a fold |
| `begin_row_lookup`, `RowQuery` | the id goes straight into the selector |
| the *miss* case in `append_activation_row` | a selection cannot miss — an out-of-vocabulary id aborts the run (§4) |
| `summarise_errors`, `assert_all_tokens_embedded`, `ActivationSequence::errors` | **only under option 2 above** |

The last row is a genuine semantic change, not a simplification: the program
loses its "which tokens failed" diagnostic in exchange for not being able to
produce a partial gather at all. The `ListLen` follow-on in §4 is what would buy
a chosen diagnostic back.

Under option 2 the five tiles become one and the schema flattens:

```text
before                                          after
main:  [recur_seq embed_prompt_token,           main:  [recur_seq embed_prompt_token]
        recur_tile summarise_errors,
        tile assert_all_tokens_embedded]
seq:   [tile begin_row_lookup,                  seq:   [tile append_activation_row]
        recur_tile scan_embedding_rows(chunk 4),
        tile append_activation_row]
```

Under option 1, `main` keeps its two trailing steps; the inner sequence flattens
the same way, which is where all the cost was.

### Cost, 12-token prompt

| | vocab 280, hidden 4 | vocab 262,144, hidden 2048 |
| --- | --- | --- |
| scan — replay units | 840 + 24 tiles | 786,432 + 24 tiles |
| scan — bytes materialized | ~150 KB | ~60 GiB |
| `BoundIndex` — replay units | 12 | 12 |
| `BoundIndex` — witness per unit | 1 index proof + 1 element proof, ~9 siblings | same, ~18 siblings |

The row itself still materializes into the append tile — 16 KB at E4B width.
What stops being paid is every *other* row.

### The lookup this does not help

The same repo's stage 1 resolves a token **string** to an id, and its merge pass
resolves a **piece pair** to a rank. Both are content-keyed, not positional, so
`scan_vocab_chunk` and the merge scan stay scans until there is a `Map<K, V>`
node. Dynamic indexes make the id→row gathers cheap and leave the string→id
lookup exactly where it is — a useful reminder of the boundary this proposal
draws.

## Open questions

- **Does the index binding need a name at all, or should it be positional?**
  `source: StorageBindingName` means the recorder must mint stable, collision-
  free names for bindings that correspond to no argument (`"row.index"` above).
  A positional form — index into a per-step vector of side bindings — avoids
  naming but makes the segment harder to read in a trace dump and couples it to
  binding order. Proposed: names, because `StorageInput` is already a name-keyed
  `BTreeMap` and the "same index used twice" case reads correctly under names.
- **Should `ListLen` ship in Phase 1 after all?** §4 argues no, on the grounds
  that the motivating programs want the gather. The counter-argument is that
  shipping an unhandleable failure mode and then adding the handling is a worse
  migration than shipping both — every program written against v1 has to be
  revisited. Worth deciding before Phase 1 lands rather than after.
- **Range with a dynamic start** — deferred, but the moment a program wants
  "the KV window ending at position p", it becomes the natural next ask. Note it
  needs more than a `BoundIndex` analogue: `ListRange` is pinned to a segment by
  `start` alone, with the width checked against the payload, so a dynamic start
  interacts with slice-width proofs in a way a single-element index does not.
