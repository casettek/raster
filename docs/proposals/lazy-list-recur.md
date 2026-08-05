# Proposal: `lazy-list-recur` — an authenticated cursor for iterating a stored list

Status: proposed 2026-08-05, revised 2026-08-05 (revision 2)

Related:
- [`bounded-collections.md`](./bounded-collections.md) — established that a `List<T>` is
  iterated, never passed whole, and enforced it at the *tile* boundary. This proposal makes
  the runtime honour the same rule at the *source*.
- [`paged-bytes.md`](./paged-bytes.md) — **depends on this.** A `Bytes` region cannot be swept
  until it lands; a sweep cannot be claimed *complete* until §5 lands; and no sweep is
  end-to-end sound until the selection-to-replay binding below lands. Three gates, not one.
- [`dynamic-index-selection.md`](./dynamic-index-selection.md) — per-item `AuthRef`s and the
  citations they carry are unchanged; §4 explains why materialization must stop dropping them.
- **Storage-selection-to-replay binding** — the generic gate `paged-bytes.md` §3.3 records.
  Per-item authorization (§4) is incomplete without it: rules 1–3 prove an item was selected,
  the replay journal proves the tile ran on *something*, and only that equality joins them.

**Revision 2 changes.** Revision 1 proposed reading the loop bound from the raster index
(`select_len`) and deferring the whole-list resolve behind a `OnceCell`. Three things were
wrong with that:

1. **It missed the earliest eager path.** `auth_ref_trace` resolves the source *before any
   runner is called* (`raster-macros/src/recur.rs:607`, `raster/src/input.rs:1687-1696`), so
   nothing downstream can be lazy.
2. **An index integer is not an authenticated bound.** `RasterIndex::validate` never
   recomputes a nested node's root, so a forged `len = 0` yields a zero-iteration sweep that
   no proof contradicts.
3. **The `OnceCell` fallback could not work.** It dropped the resolve closure that would
   populate it, and for a non-indexed source the length query fails *before* item zero, so
   the fallback could never fire.

Revision 2 replaces all three with one mechanism: a compact **authenticated list metadata**
payload that proves `(len, elements_root)` without touching an element, and an always-indexed
**`ListCursor`**. The same metadata that authenticates the loop bound is what makes tracing
O(1), so items 1 and 2 are one change, not two. The `OnceCell` question disappears with the
fallback it was guarding: there is no unindexed recur source (§3).

It also promotes **completeness auditing** from an adjacent gap to in-scope (§5), carried by
one new typed field on `TileReplayJournal` rather than by parsing output bytes or normalizing
the recur ABI.

**The settled decision on recur sources.** `call_recur!` and `call_recur_seq!` both require a
raster-indexed `List`; there is no materializing fallback. Internally stored values already qualify unconditionally
(`storage.rs:1004`), so the rule binds only external inputs declared `encoding = "postcard"`,
which fail at `open` with a re-encode message. A survey of this repository found nothing to
migrate — the reference example already sources its sweeps from a raster-encoded input (§3).

## Problem

`call_recur!` over a storage-backed list materializes the **entire list** before the first
iteration runs. The rule that a collection never crosses a tile boundary whole is enforced by
the type system; the rule that it is never *held* whole on the host is enforced nowhere, and
the runtime breaks it on every recur.

There are **three** eager paths, in execution order.

### 1. Tracing, before any runner

The generated recur wrapper opens with:

```rust
// raster-macros/src/recur.rs:607
let __raster_input_trace = ::raster::auth_ref_trace(&input)…
```

and `auth_ref_trace` (`raster/src/input.rs:1687-1696`) resolves the binding:

```rust
AuthRef::Storage(binding) => {
    let resolved = (binding.resolve.as_ref())(binding.reference.clone())?;
    Ok(AuthRefTrace {
        value: FnInputValue::StorageBinding,
        storage: Some(TraceStorageData {
            coordinates: resolved.reference.coordinates,
            commitment:  resolved.reference.commitment,
            selector:    resolved.selector,
            selection:   resolved.selection,
        }),
        …
```

Note what it keeps: **not the value.** `resolved.value` is dropped. The resolve exists to
obtain `selection` — a `SelectionCommitment` whose `selected_hash` is the hash of the
selected payload. For a whole-list selector that payload is the entire list, so the cost is
real and cannot be removed by "just skipping the resolve." It can only be removed by giving
a recur source a *cheaper authenticated selection*, which is §1 of the design.

### 2. The runners

Recur **tiles** (`run_recur_list*`, `raster/src/input.rs:1990`, `:2038`, `:2083`) call
`resolve_recur_list` (`:1364-1377`) and iterate an owned `Vec<T>`; items are never selected
individually and the index is not consulted once.

Recur **sequences** (`run_recur_sequence_list*`, `:2139`, `:2188`, `:2234`) use
`resolve_recur_list_source` (`:1281-1300`), which resolves the parent behind an `Rc` and then
builds each item with `select_item` (`:1309-1348`). That per-item path *is* index-driven — so
the machinery for lazy iteration already exists on one family and is absent on the other.

### 3. Chunking

`chunk_auth_ref` (`:1407-1435`) wraps the source's resolve closure to resolve the whole list
and then copy it again into `Block`s. It deliberately preserves the selector (`:1398-1405`),
which is right for provenance and fatal for memory.

### What the eager resolve costs

The parent resolve goes through `StorageManager::select` (`storage.rs:525-552`), which for a
whole-list selector makes three full-size passes: `tree_value_from_raster_location` builds a
`TreeValue` owning a copy of every element; `selected_payload_from_raster_location` does
`.to_vec()` over the whole subtree payload; `typed_value_from_tree` deserializes it into
`Vec<T>`. For a 1 GiB source that is roughly **3 GiB of peak host memory before the first
tile runs**, held for the loop's duration.

### Why the index alone cannot supply the bound

`RasterIndex::validate` (`raster_index.rs:322-406`) checks the format version, that the
**root** node's hash equals `root_commitment`, and per-node shape (`len == elements.len()`,
Merkle level widths). It never recomputes a nested node's `root_hash` from its children, and
`locate` returns `root_hash: self.root_commitment` (`:217-222`) — the root's hash, not the
selected node's. So a nested list node's `len` is index-trusted, full stop.

A forged index can therefore claim `len = 0`, `elements = []` for a nested list. The sweep
emits zero iterations, no item selection proof is ever produced, and nothing folds that zero
against the committed list root.

**This is pre-existing, not introduced here.** Today's eager path reads the same forged index
— `tree_value_from_raster_location` walks the same empty `elements` and yields an empty
`Vec<T>`. Lazy iteration does not regress it. But `paged-bytes` turns "iterate a list" into
"prove you swept an entire artifact," which makes the gap load-bearing rather than latent, so
it is in scope here (§5) rather than filed away.

## Design

### 1. Compact authenticated list metadata

A new terminal payload form that proves a list's length and element root without carrying
elements:

```text
0x0A ‖ len:u64 ‖ elements_root:32          (len > 0)   1 + 8 + 32 = 41 bytes
0x0A ‖ 0u64                                (empty)     1 + 8      =  9 bytes

root = H(b"list-root" ‖ len ‖ elements_root)      // len > 0
     = H(b"list-root" ‖ 0   ‖ b"empty")           // len == 0
```

That is exactly the final step of `list_root_from_hashes` (`raster-core/src/input.rs:352-376`),
including its empty-list sentinel. `parse_subtree_root` gains one arm that recomputes the
hash and returns it; the enclosing `Struct` steps then fold as they do for any other child,
so `verify_selection_proof` and `step_proves_segment` are untouched and the step/segment
bijection is preserved (a metadata selection has the same segments as selecting the list).

**Why this authenticates the bound.** The root is *recomputed* from `(len, elements_root)`,
so producing a valid metadata payload for root `R` requires knowing a preimage of `R`. A
forged length yields a different root and fails to fold. This is strictly stronger than the
`0x09` list handle, which returns its *stored* root and ignores its stored `len` (`:441-452`)
— a handle header carrying `‖ R ‖ 999 ‖ 0` verifies to the same `R`, so the handle's length
field is not authenticated and cannot be used for this.

**The consumer pins the payload kind; the selector does not.** A metadata selection and a
whole-list selection share a path and differ only in payload — `0x0A` against `0x02`. That is
correct for the proof (both fold to the same root, and the metadata form's root is
*recomputed*, so it is the stronger of the two), and it is not a selector concern: a segment
names a descent, and metadata is a view of one node rather than a step below it.

What is required instead is that **every consumer states the payload kind it expects**, and the
recur-source binding requires `0x0A`. Accepting a `0x02` payload there would not forge `L` —
the full-list fold authenticates its own length too — but it would put the whole-list payload
back in the trace, which is the `O(list)` cost this proposal removes, and it would make the
audit's parse path depend on prover-chosen data. The transition check therefore rejects a
non-`0x0A` payload at a recur-source binding outright rather than falling back to parsing it.

Other consumers already pin their kinds implicitly — a range proof calls
`parse_list_child_roots`, which requires `0x02` and fails on `0x0A`. Stating the rule generally
is what keeps that true for payload kinds added later.

**Producing it costs nothing.** `elements_root` is `merkle_levels.last().hashes[0]`, and
`RasterIndex::validate` already guarantees that last level holds exactly one hash
(`raster_index.rs:376-380`). The empty case is equally direct — validate requires
`merkle_levels.is_empty()` when `len == 0`. So metadata comes entirely from the index, in
O(1), with no data-file access. **The `.rindex` format does not change and `rindex02` stays
readable.**

**Tag allocation is central, not negotiated per proposal.** The space is contiguous and small
enough that two documents each reserving a byte is how collisions happen. The canonical list
belongs in `parse_subtree_root`'s doc comment (`raster-core/src/input.rs:378`), added by
whichever change lands first:

| tag | payload | defined by |
| --- | --- | --- |
| `0x00` | leaf | existing |
| `0x01` | struct | existing |
| `0x02` | list | existing |
| `0x03` | unit | existing |
| `0x04` | map | existing |
| `0x05`–`0x08` | enum unit / newtype / tuple / struct | existing |
| `0x09` | list handle (stored root, skipped body) | `bounded-collections.md` |
| `0x0A` | **list metadata** | this proposal |
| `0x0B` | **bytes page** | `paged-bytes.md` |

### 2. Recur tracing consumes metadata

Replace the recur wrapper's `auth_ref_trace(&input)` with a metadata-producing path: the
recorded `TraceStorageData` for a recur source carries the same `coordinates`, `commitment`,
and `selector`, and a `SelectionCommitment` whose payload is the metadata rather than the
whole list: `1 + 8 + 32 = 41` bytes for a non-empty list, `1 + 8 = 9` for an empty one. So
`selected_hash` is over those bytes and `selected_len` is 41 (or 9), against a `selected_len`
of the entire list payload today.

This is what actually moves peak memory. Everything else in this proposal is a consequence.

### 3. `ListCursor` — always indexed, never materialized

> **Invariant: every recur source is raster-indexed, and no recur ever materializes its
> source whole.** There is no second path, so there is no path that reintroduces `O(list)`.

This binds **both** recur macros: `call_recur!` and `call_recur_seq!` open a `ListCursor` over
the same `AuthRef<List<T>>`, so the indexed requirement and the `open` error below apply
identically to each.

There are **three** drivers, not four — `call_recur_seq!` has no chunking:

| driver | element | chunked |
| --- | --- | --- |
| `call_recur!` (recur tile) | yes | yes (`chunk = N`) |
| `call_recur_seq!` (recur sequence) | yes | **not supported** |

`RecurCallInput` carries `chunk: Option<Expr>` (`raster-macros/src/lib.rs:1451`, parsed at
`:1504`); `RecurSequenceCallInput` (`:1457-1463`) has no such field, so the key does not parse.
The driver-level range work in §6 therefore applies to recur tiles only. Adding chunking to
`call_recur_seq!` is a separate change and out of scope here.

```rust
struct ListCursor<T> {
    reference: StorageRef,
    selector: SelectorPath,
    len: u64,                         // authenticated, from §1
    index: Arc<RasterIndex>,          // retained; never re-parsed per item
    source: SourceHandle,
    index_bindings: Vec<IndexBinding>,
}

impl<T> ListCursor<T> {
    fn open(source: &AuthRef<List<T>>) -> Result<Self>;   // errors if not indexed
    fn metadata(&self) -> AuthenticatedListMetadata;      // (len, elements_root, commitment)
    fn select_item(&self, index: u64) -> Result<AuthRef<T>>;
    fn select_range(&self, start: u64, end: u64) -> Result<AuthRef<Block<T>>>;   // chunked recur
}
```

**There is no `Materialized` backend.** An earlier draft carried one for "a postcard-backed
internal object," which is a case that does not exist: `store_value_at_coordinates` sets
`raster_payload = Some(raster_payload_for_value(value)?)` unconditionally
(`raster-runtime/src/storage.rs:1004`, `:148-155`), so every internally stored value — every
tile output, every finalized draft, every `store_value` — already carries a raster tree and
index alongside its postcard bytes. Internal objects are always `Indexed`.

**Migration scope, surveyed.** In this repository: nothing to migrate. `ExternalEncoding`
defaults to `Postcard` (`raster-core/src/input.rs:1033-1034`), so an input with no `encoding`
key is postcard — but the reference example already keeps the two apart deliberately.
`examples/hello-tiles` declares `personal_data` as postcard and `personal_data_bin` as
`{ path: "personal_data.rastered", index_path: "personal_data.rindex", encoding: "raster" }`
(`bin/gen_input.rs:52`, `:63`), and every one of its four `call_recur!` sites sources from
`address_lines`, selected from the **raster** binding at `src/main.rs:107`. The postcard input
feeds only scalar `select!`s and a `call_seq!` argument. So the example already models the
pattern this rule requires, and `references/examples.md` — which the skill points authors at —
already teaches it. The rule makes an existing convention enforceable rather than introducing
a new constraint.

The one source that genuinely lacks an index is an **external input declared with
`encoding = "postcard"`**: `input.bin` with no `.rindex`. Postcard is sequential and not
self-indexing, so `rows[i]` cannot be located without decoding everything before it. Keeping a
fallback for that case would preserve compatibility by preserving exactly the `O(list)` peak
this proposal exists to remove — on the one input class where the list is most likely to be
large, since it came from a file. It fails loudly instead:

```text
call_recur! requires a raster-indexed List source;
re-encode this input with encoding = "raster"
```

That is a real restriction on existing programs, and the right one: large iterable data must be
imported as Raster. It is also the same shape of restriction `bounded-collections.md` already
imposes — the model refuses to express the unbounded case rather than servicing it slowly.

Two further properties revision 1 lacked:

- **Errors propagate; nothing falls back.** Today's `select_item` closure is
  `if let Ok(selected) = select_stored_value(…) { … } else { select_storage_value(…) }`
  (`:1336-1343`), which treats a malformed index and an out-of-range selector identically to
  "this source has no index" — converting a corruption signal into a multi-gigabyte
  allocation. With no `Materialized` variant there is nowhere to fall back *to*, so the
  distinction the earlier draft had to draw carefully now holds by construction: "no index" is
  refused at `open`, and every later failure is a failure.
- **The parsed index and file handle are retained.** Owned raster objects currently re-parse
  the index on every `select` (`storage.rs:534`, `backing.rs:330`); over 100 000 items that
  is 100 000 index deserializations. (External raster sources already cache behind an `Arc`
  — `source/file.rs:236`, test at `:532` — which is the `paged-bytes` path, so this is a
  correctness-of-design fix for owned objects rather than a hot-path fix for regions.)

### 4. Route both recur families through the cursor

Recur tiles currently bypass per-item selection entirely. Move them onto the cursor so one
implementation serves both families.

**State the authenticated relationship first.** Revision 1 proposed "trace equality" as the
acceptance criterion, which tests that nothing changed rather than that the right thing is
authenticated. The intended relationship is:

1. the **source** is authenticated once, by the metadata selection: `(len, elements_root)`
   folding to the committed list root;
2. each **item** is authenticated by its own selection proof at `source[i]`, carrying any
   citations inherited from the source binding;
3. each iteration's `RecurInput.len` equals the authenticated source length;
4. the value the tile **replayed on** is the value the item proof **selected** (see the
   release gate below).

#### Materialization must not collapse two outputs into one

`build_recur_input` (`:1352-1362`) resolves an authorized item and throws away everything
except the value:

```rust
let value = into_auth_value::<T, _>(item)?.into_inner();
Ok(RecurInput::new(value, index, len))
```

`into_auth_value` produces an `AuthValue::Storage(StorageValue { reference, selector,
selection, value, … })`; `.into_inner()` keeps `value` and discards the selector, the selection
commitment, and any inherited citations — before the iteration trace can record them. So
"rewrite it to preserve citations" understates the fix: materialization has **two** outputs and
the runner needs both.

```rust
/// Host-side only. Nothing here crosses the tile boundary.
struct MaterializedRecurItem<T> {
    /// The only part the tile sees — unchanged ABI.
    replay_input: RecurInput<T>,
    /// Recorded in the iteration's `FnInput`/storage witnesses.
    trace_binding: TraceStorageData,
    /// Citations inherited from the source binding's path.
    index_bindings: Vec<IndexBinding>,
}
```

used as:

```rust
let item = cursor.select_item(index)?;
let built = materialize_recur_item(item, index, len)?;
recorder.record_item_binding(built.trace_binding, built.index_bindings);
let result = step(built.replay_input, state);
```

The struct is illustrative — a tuple or a trace-builder callback works equally well. What
matters is that the two outputs are not accidentally collapsed, which is the whole of the
current bug.

**This shape is the established one, not a new invention.** `IndexSource::resolve_index`
returns `(u64, TraceStorageData, Vec<IndexBinding>)` (`raster/src/input.rs:1113`, impl at
`:1133-1146`) — value, binding, citations — and `program_output_binding` returns
`(TraceStorageData, T)` (`:1723-1743`). Both solve exactly this problem: materialize an
authorized reference while keeping what authorizes it. `build_recur_input` is the outlier. It
is unreferenced anywhere in the workspace today, so there is no caller to migrate.

**Why the tile ABI stays fixed.** The tile still receives `RecurInput<T> { value, index, len }`
and nothing else — no selector paths, no proofs, no storage handles. The replay guest does not
need them; the *transition* guest does, and it receives them separately as the step's storage
witnesses. Keeping the carrier host-side is what lets per-item provenance be recorded without
touching the replay ABI or the image ids that pin it.

#### Citations are why this matters even without a dynamic index

If the source list was itself reached dynamically:

```rust
let rows = select!(List<Row>, tables[table_id]);
call_recur!(tile = f, input = rows, …);
```

every item path inherits the citation: `tables[BoundIndex(table_id)].rows[Index(i)]`. The
iteration's step must therefore carry the storage binding that authorized `table_id`, or
`verify_bound_index_bindings` (`trace.rs:141-181`) rejects the item selection outright for a
missing cited source — `BoundIndexViolation::MissingSource`. Dropping `index_bindings` during
materialization does not weaken the audit; it makes a legitimate program un-auditable.

#### Release gate: item selection root == replayed value root

Rules 1–3 prove *an item was selected*, and the replay journal proves *the tile ran on some
recorded input*. Neither says they are the same value. With a committed list `[A, B, C]` and
iteration 1, the audit as specified accepts:

```text
proved:   B is the committed element at source[1]
proved:   the tile replayed on the recorded input bytes
missing:  those bytes decode to B
```

Both statements also hold if the tile ran on `C`. Closing it needs the generic equality

```text
item selection proof root  ==  structural root of the decoded RecurInput.value
```

which is exactly the storage-selection-to-replay binding that `paged-bytes.md` §3.3 lists as
its release gate. It is **not** specific to recur or to `Bytes` — it affects every Rastered
value — but per-item authorization here is incomplete without it, so this proposal names it as
a dependency rather than assuming §4 delivers end-to-end authorization on its own.

### 5. Completeness auditing

With an authenticated `len`, the sweep can be held to it. A valid early exit and a
prover-truncated trace are indistinguishable from outside the guest, so the audit needs two
facts per iteration — where the iteration sat in the source, and how it terminated — and both
must be **replay-proven**, not host-recorded.

#### The iteration facts travel in the replay journal

Neither fact is recoverable from bytes the guest already parses. `RecurInput` is
`{ value: T, index: u64, len: u64 }` (`raster/src/input.rs:91-95`) — `value` comes **first**,
so `index` and `len` sit behind an arbitrary-length `T` and cannot be reached without a full
decoder for user types inside guest audit code. (The chunk length escapes this only because
the chunk vector *is* the leading field, which is exactly the layout assumption
`chunking.rs:1-9` documents and depends on.) Termination has the same problem from the other
side: the tile's return type varies by recur mode, so no fixed offset in `output_bytes` holds
the control discriminant.

So the tile guest commits both directly:

```rust
// raster-core/src/draft.rs
pub struct TileReplayJournal {
    pub input_commitment: [u8; 32],
    pub output_bytes: Vec<u8>,
    pub draft_transition: Option<DraftReplayTransition>,
    /// `Some` for every recur tile, `None` for every other tile.
    ///
    /// Membership is by **recur site**, not by subtree. An ordinary tile in a recur
    /// sequence's own body emits `None` — it carries no `RecurInput`, and the sequence
    /// cannot terminate early (`recur.rs:317`), so that site's completeness comes from
    /// trace structure. A `call_recur!` *nested* inside such an iteration is a different
    /// site: its tiles emit `Some`, attributed to the nested site by their own
    /// coordinates (see rule S5).
    pub recur: Option<RecurTileReplay>,
}

pub struct RecurTileReplay {
    pub position: RecurPosition,
    /// Always explicit. A recur tile whose return type carries no `RecurControl` emits
    /// `Continue` rather than omitting the field — see below.
    pub control: RecurControlKind,
}

pub struct RecurPosition {
    pub iteration_index: u64,
    /// `RecurInput.len` — the number of iterations the tile was told to expect, **not**
    /// the source length. The two differ under `chunk = N`, and the name must not paper
    /// over that: `RecurInput::is_last()` is `index + 1 == len` (`input.rs:180-182`), so
    /// `len` is load-bearing user-visible semantics that must stay the iteration count.
    /// The audit relates it to the authenticated source length itself, by rule 3.
    pub declared_iterations: u64,
    /// Elements this iteration consumed: 1 for element recur, the chunk size for chunked
    /// recur (§6), short on the final chunk. Replaces `chunking::iteration_chunk_len`'s
    /// leading-varint inspection outright.
    pub consumed_elements: u64,
}

pub enum RecurControlKind { Continue, Break }
```

**One nested option, not two flat ones.** An earlier draft had `recur_position` and
`recur_control` as independent fields, on the theory that the two facts had different carriers
— position from a recur tile's journal, control from whichever tile inside a recur *sequence*
produced it. The `recur.rs:316-318` finding removed that second carrier: recur sequences have
no control at all. The split outlived its reason and left three states representable that
cannot occur — `(None, Some)`, `(Some, None)`, and the question of what each means. Nesting
makes them unspellable at no cost: `Option<RecurTileReplay>` is one byte when absent, against
two for a pair of `None`s.

**`control` is required, so the audit never applies a default.** Recur tiles come in modes
whose return types carry no `RecurControl` — an output-only step returns `RecurOutput<S>` and
always continues. The earlier shape expressed that by *omitting* the field and reading absence
as `Continue`, which put a default in audit code: exactly the shape of the `InputSource::Inline`
permissive fallback that `dynamic-index-selection.md` records as a soundness hole. The wrapper
knows the return kind statically, so it emits `Continue` explicitly and the audit reads a value
rather than an absence.

**Why the journal and not a recorder marker.** The journal is postcard-encoded and passed to
`env::verify(image_id, journal_bytes)` (`checks/io.rs:94-97`), so everything in it is covered
by the replay receipt. A host-recorded marker is host-supplied and proves nothing.

**Why this is cheap to build.** The journal is assembled in the macro-generated wrapper
(`raster-macros/src/lib.rs:1112-1131`), which holds the typed `result`. The existing
`gen_replay_transition_binding` already matches
`RecurControl::Continue((_, draft)) | RecurControl::Break((_, draft))` and *discards the
distinction* — splitting that arm is the whole of `control` for the modes that have one, and
the modes that do not get a literal `Continue` from the same `ProtocolReturnKind` match.
`position` must be captured from the typed `RecurInput` **before the call**, not where the
journal is built: the decoded args (`:1231`, `:1238`) are moved into the tile at
`let result = #target_fn(#(#param_names),*)` (`:1412`), and `RecurInput` is not `Copy`, so by
journal-construction time (`:1112-1131`) they are gone. The wrapper emits a two-field capture
between decode and call for recur-tile kinds only. This is a one-line codegen ordering
constraint, but the natural place to write journal-building code is where the journal is
built — which is exactly where it will not compile.
`Option<DraftReplayTransition>` is the precedent for a per-mode optional journal field; `None`
costs one byte.

**The journal field is a binding, not an authority.** It commits what the tile *saw* in its
input; committing a value does not make it true. Its role is exactly `input_commitment`'s —
"this is what ran" — and authority comes from checking it against the authenticated source
metadata of §1.

#### Completeness rules

Given the authenticated source length `L` from §1 and the per-iteration journals:

1. the first iteration has `iteration_index == 0`;
2. iteration indices are contiguous;
3. every iteration's `declared_iterations == ⌈L / C⌉`, where `C` is the CFS-declared chunk
   size (1 when unchunked). This is where the tile's view of the loop is tied to the
   authenticated source length; `C` is a CFS literal and therefore already in program
   identity;
4. every iteration begins with `covered_before < L` and consumes **exactly** the shape the
   program declared — including a terminating `Break`:

   ```text
   expected_consumed = min(C, L − covered_before)
   consumed_elements == expected_consumed
   ```
5. a terminal `Continue` requires the prefix to be **complete**: `covered_end == L`.
   Equivalently, `Continue` must be followed by a further iteration unless the source is
   exhausted;
6. `Break` must be terminal, and permits an **incomplete** prefix (`covered_end <= L`); it is
   attributed to the loop whose iteration it ends — never to an enclosing loop (see recur
   sequences below);
7. zero iterations are valid **iff** `L == 0` — and `L > 0` with zero iterations is rejected.
   This is the forged-`len = 0` sweep, and the case the whole mechanism exists for;
8. **chunked recur only** — each iteration's range selection must agree with its journal:
   `ListRange.len == L`, `ListRange.start == covered_before`, and the payload's element count
   `k == consumed_elements` (§6).

**Coverage is a prefix, not a tiling.** Rule 4 said "covers `[0, L)`" in an earlier draft,
which contradicted rule 6 outright: a legitimate mid-sweep `Break` necessarily leaves
`covered_end < L`, so no trace could satisfy both. Splitting the invariant (always a
contiguous prefix) from the terminal condition (complete on `Continue`, free on `Break`) is
what makes an early exit expressible without also excusing a truncated one. For unchunked
iteration `i`, coverage after that iteration is `[0, i + 1)`.

**`Break` stops future iterations; it never shrinks the current one.** An earlier draft
exempted the terminal iteration from the shape requirement, which let a prover pick both the
chunk size *and* the stopping point. With `L = 100`, `C = 4`, a single iteration consuming 1
element and returning `Break` passed every rule: `declared_iterations == 25` ✓, non-zero ✓,
terminal so the shape clause did not apply ✓, prefix `[0, 1)` permitted by rule 6 ✓, and even
rule 8 agreed, because the range selection honestly *was* one element. The tile ran on a
1-element block while the program declared `chunk = 4`.

The exemption existed to allow a short **final source chunk**, which `min(C, L − covered_before)`
expresses directly. Making the equation unconditional separates the two decisions cleanly: how
much *this* iteration sees is the program's, and whether there is a *next* iteration is the
tile's.

**One equation replaces a rule plus two exemptions.** `min(C, L − covered_before)` subsumes:

- *progress* — `covered_before < L` implies `expected_consumed ≥ 1`, so "non-zero" needs no
  separate clause;
- *the ceiling* — the running total cannot pass `L` by construction;
- *chunk ordering* — a chunk is short exactly when `L − covered_before < C`, i.e. only the final
  source chunk. The `4,1,4,1` shape at `C = 4, L = 10` is now rejected at iteration 1, which
  expects 4, rather than needing a separate ordering rule. This is the guest-side equivalent of
  `check_previous_chunk_was_full` (`chunking.rs:83`), which today runs only in the native
  recorder (`raster-runtime/src/tracing/recorder.rs:778`) while the transition guest checks only
  the per-iteration bound (`checks/cfs.rs:117-158`). `consumed_elements` is what lets the guest
  check it at all, and with it both `iteration_chunk_len` and `leading_varint` become dead and
  should be deleted **in this phase**, not left to a later one.

**Gaps and overlaps are unrepresentable, so rule 4 does not mention them.** An earlier draft
had it forbid both, which read as diligence and was noise: `consumed_elements` is a *span with
no start*, so the covered range is derived by accumulation — `[running_total, running_total +
consumed)` — and there is no way to express a range that skips or repeats. Stating a check for
an impossible state hides which checks are load-bearing. This holds only while
`consumed_elements` carries no start field — if one is ever added, the clause has to come back.

**Rule 5 pins where the prefix ends**, which rule 4 does not. Rule 4 fixes the size of every
iteration that exists; it says nothing about how many exist. With `C = 4`, `L = 10`, a trace of
two iterations consuming `4, 4` and ending in `Continue` satisfies rules 2, 3 and 4 completely —
both chunks are exactly the declared shape — and stops at `covered_end = 8`. Dropping the tail
by simply running fewer correctly-shaped iterations is what rule 5 exists to see, and it is the
only rule that does.

**Rules 1–8 above are recur-tile rules and apply to nothing else.** Recur sequences have no
`recur` journal at all — the tiles of a sequence's own body never receive a `RecurInput` — so
every rule that reads
`position` or `control` is meaningless there, and even the two that look reusable (indices
start at zero, indices are contiguous) are read from a different place. Recur sequences get
their own self-contained set below rather than a subset of these. Reusing numbers across two
mechanisms is what produced the earlier draft's claim that recur sequences satisfy "rule 3",
which reads `declared_iterations` from a journal they do not emit.

**The two modes are pinned by different mechanisms**, which is why rule 8 is scoped. Unchunked
iterations are reached by `Index(i)`, so position is pinned by the index step of the item's own
selection proof and `consumed_elements` is always 1 — carried anyway, so the rules stay uniform
and the field never needs to be optional. Chunked iterations are reached by a range selection,
which carries the span, so rule 8 has something to cross-check against. Neither mode leaves
`consumed_elements` as the sole authority for where an iteration sat.

#### Recur sequences: no termination fact exists

`TileReplayJournal` is per *tile*, and a `call_recur_seq!` iteration is a sequence invocation
containing several tile steps — possibly **none** that ever sees a `RecurInput` for *this*
site, since `into_ref!` exists precisely so the item stays an `AuthRef` and is never
materialized. (An iteration may still contain a nested `call_recur!`, whose tiles do emit a
`recur` journal — attributed to that inner site, not this one. Membership is by site
throughout.) That looks like a carrier problem for both facts. It is not, because **a recur
sequence cannot terminate early at all**:

```rust
// raster-macros/src/recur.rs:317
panic!("`#[sequence(kind = recur)]` cannot return `RecurControl`; \
        early termination must be decided inside recur tiles");
```

Rejected at macro expansion. `Break` is a recur-*tile* feature (see
`examples/hello-tiles/src/lib.rs:195-211`, a state+output recur tile), so the `recur` journal is a
recur-tile-only journal field and rule 6 simply does not apply to recur sequences. Their
completeness collapses to the strongest and simplest form:

> **A recur sequence's observed iteration count must equal `L`.** Rules 5 and 6 do not apply.

This also disposes of an attribution problem that would otherwise be real. A recur-sequence
iteration may contain a nested `call_recur!`, and a tile inside *that* loop can `Break`:

```text
[2]              outer loop (call_recur_seq!)
[2][1]             iteration 1
[2][1][3]            nested call_recur! site
[2][1][3][7]           iteration 7  ← this tile returns Break
```

A containment-based rule ("some tile inside the iteration said `Break`") would read that as
the outer loop terminating legitimately — handing the truncation this whole section exists to
catch a free pass. It cannot arise: the inner `Break` is attributed to `[2][1][3]` by its own
coordinates, and the outer loop has no control to confuse it with.

**Position is structural, and needs one widening.** Iterations are addressed at `site ++ [i]`,
so the iteration index *is* the last coordinate — nothing has to be committed for the audit to
know where an iteration sat. `try_get_recur_iteration_coordinates`
(`raster-core/src/cfs.rs:412-423`) is exactly that decomposition, but its `matches!` accepts
only `SequenceChildItem::RecurTile(_)` and returns `None` for a recur-sequence site. Widening
it to `RecurTile(_) | RecurSequence(_)` is a one-line change with precedent two lines below:
`expand_recur_entry_coordinates` (`:425-436`) already matches both.

One trap in the addressing: a step inside recur-sequence iteration `i` sits at
`site ++ [i] ++ [j]`, so splitting *its* last coordinate yields `j`, the step index. It looks
plausible and is wrong. Position must be read at the iteration's own boundary events, which
the `#[sequence]` wrapper emits at `site ++ [i]`.

#### Recur-sequence completeness rules

Stated independently of the recur-tile set, because they share no mechanism with it — these
are read entirely from trace structure, with no journal involvement whatsoever:

**S1.** the source metadata authenticates `L` (§1);
**S2.** every iteration has paired Start/End boundary events at `site ++ [i]`;
**S3.** iteration boundary indices start at zero and are contiguous;
**S4.** the observed iteration count equals `L`;
**S5.** a `Break` inside a `call_recur!` nested within an iteration is attributed to that inner
site by its own coordinates and has no bearing on this one.

That is the whole set. Two things it does *not* need, both because the runner has no early
exit (`raster/src/input.rs:2147-2148`, `:2164-2173`):

- **No prefix rule.** S3 and S4 together pin the observed indices to exactly `{0, …, L-1}`, so
  a short sweep fails S4 with nothing available to excuse it. Where a recur tile needs the
  prefix/terminal split to keep `Break` legal, a recur sequence has no such case.
- **No separate empty-source rule.** `count == L` covers `L == 0` in both directions, so the
  recur-tile set's "valid iff" phrasing has no analogue here.

#### Why `control` is required rather than defaulted

For recur *tiles*, `IntoRecurControl` has two impls (`raster/src/input.rs:143-153`): the
identity on `RecurControl<T>`, and a blanket `impl<T> IntoRecurControl<T> for T` yielding
`RecurControl::Continue(self)`. A tile returning a plain value therefore always continues.

An earlier draft encoded that by omitting the field and reading absence as `Continue`. It is
tempting — the information really is redundant with the return type — and it is the wrong
place to put it. The absent case would be interpreted in guest audit code, where a default is
indistinguishable from a field a malicious or merely buggy producer failed to set, and where
`dynamic-index-selection.md` has already recorded what that costs: `InputSource::Inline` was
the permissive fallback that let an unattributed form pass with an unchecked index.

The redundancy is free to resolve at the *producer*, where the return kind is known statically:
the wrapper's `ProtocolReturnKind` match emits `Continue` for the modes that cannot break and
the discriminated value for the modes that can. The audit then reads a value in every case and
rule 6 has something to check on every iteration, including sweeps with no early exit.

### 6. Driver-level chunking

Chunking must move into the driver rather than remaining a type-changing adapter:

```text
source length   = n                       (authenticated)
iteration count = ⌈n / chunk⌉
iteration i     = cursor.select_range(i*chunk, min((i+1)*chunk, n))  ->  Block<T>
```

`AuthRef<List<Block<T>>>` cannot be the source type. The storage tree contains `List<T>`:
length `n`, element proofs for `T`. A list *of blocks* does not exist in it, so there is
nothing to prove membership in. Today's `chunk_auth_ref` survives only because it never
proves the synthetic list — it regroups a resolved value while preserving the parent selector.
Once chunks come from range selections, the driver owns the count and the per-iteration range.

This needs index-driven range selection, which does not exist: `raster_index.rs:207` rejects
`SelectorDescent::Range` for raster-encoded inputs outright. The verifier side is already
built — `SelectionProofStep::ListRange`, `fold_list_range`, and `parse_list_child_roots`
(`raster-core/src/input.rs:255-263`, `:739-751`) — so the work is `RasterIndex::locate`/`select`
learning to descend a range and emit the boundary siblings.

#### The range proof independently pins the sweep

`ListRange { start, len, siblings }` proves "the payload is the slice `[start, start + k)` of a
list of `len` elements, where `k` is the payload's own element count" (`input.rs:255-258`), and
`fold_list_range` already rejects `start + k > len` (`:662`). So for chunked recur the proof
carries the same three facts the journal does:

| fact | journal | selection proof |
| --- | --- | --- |
| source length | — (`L` from §1 metadata) | `ListRange.len` |
| where the iteration sat | running total of `consumed_elements` | `ListRange.start` |
| how much it took | `consumed_elements` | payload element count `k` |

Rule 8 requires all three to agree. That matters because §5 is explicit that the journal is a
*binding, not an authority*: on its own, `consumed_elements` is a value the guest committed
having seen it in its input. Cross-checking it against a folded proof makes the coverage
argument rest on something the prover cannot choose freely — and `ListRange.len == L` closes
the loop back to the authenticated metadata, so the two independent paths to `L` must meet.

#### Why this cannot be deferred

Revision 2 listed chunking as an optional later phase. That is wrong: **§1–§5 without it
break chunked recur, which is live in the repository** (`examples/hello-tiles/src/main.rs:125`,
`chunk = 2`).

`chunk_auth_ref` hands the runner an `AuthRef<List<Block<T>>>` while preserving the selector
of the underlying `List<T>` (`raster/src/input.rs:1407-1435`). Type and path already disagree;
today nothing notices, because the value is regrouped after a whole-list resolve. Make the
cursor index-driven and the disagreement becomes load-bearing:

- metadata authenticates `L` **elements**, while `RecurInput.len` is the **chunk count**;
- `cursor.select_item(i)` reaches element `i` of the real list and yields a `T`, not a
  `Block<T>`;
- rule 3 would compare two different quantities;
- the cursor cannot prove membership in a `List<Block<T>>` that does not exist in the tree.

Chunked recur also survives today only on the machinery phase 4 removes: `chunk_auth_ref`
regroups a **fully resolved** list, and the recur-tile runners then iterate that owned `Vec`
(`resolve_recur_list`, `:1364-1377`). Delete the whole-list resolve and the synthetic
`List<Block<T>>` has nothing to be built from. The escape hatch closes in the same change that
makes the disagreement matter.

So chunking lands with the rest. `chunk_auth_ref` is **deleted** rather than deprecated, the
source type stays `AuthRef<List<T>>` in both modes, and the generated wrapper's bound becomes
`IntoAuthRef<List<T>>` uniformly (`raster-macros/src/recur.rs:592-599`, where `#item_ty` is
`Block<T>` today). No synthetic list is ever constructed.

**The authoring surface does not change.** `call_recur! { …, chunk = N }` is spelled the same,
the step still takes `RecurInput<Block<T>>`, and `index`/`len`/`is_first`/`is_last` keep their
current meanings — which is what makes landing it together affordable rather than a migration.

## Soundness

- **The loop bound becomes authenticated** where it was previously index-trusted. This is the
  proposal's main soundness contribution.
- **The sweep is held to that bound.** An authenticated `L` closes nothing on its own — a
  prover can still stop early. §5's replay-proven iteration facts are what turn "the source
  has `L` elements" into "this trace covered them," and only the pair lets `paged-bytes`
  describe a sweep as covering an entire region. The forged-`len = 0` sweep needs both: §1 to
  make the zero unprovable, §5 rule 7 to reject the zero-iteration trace.
- **Metadata is strictly stronger than the existing handle.** Recomputation from
  `(len, elements_root)` versus the `0x09` handle's stored-root-and-ignore-len.
- **Two payload forms may verify to one root** (full list `0x02`, metadata `0x0A`). This
  mirrors the handle's design and is safe for the same reason plus preimage resistance; see
  §Uncertainties for whether the two should nonetheless be distinguishable by path.
- **Per-item authorization is unchanged.** Item selection proofs, `BoundIndex` citations, and
  the cycle graph are exactly as `dynamic-index-selection.md` ships them.

### What this proposal proves, and what it does not

With everything here landed and the selection-to-replay binding **not** yet landed:

| claim | status |
| --- | --- |
| the source list has exactly `L` elements | proved (§1, authenticated metadata) |
| the sweep visited indices `0..k` contiguously, with the declared chunk shape | proved (§5) |
| the sweep covered the whole source, or stopped at a replay-proven `Break` | proved (§5) |
| each visited index names a committed element | proved (per-item selection proof) |
| **each tile consumed the value at the index it claims** | **not proved** |

The last row is the whole of the gap: the cursor and completeness rules establish *which
indexes were traversed*, not that each replay ran on the selected value. A prover who
substitutes element `C` where the proof selects `B` produces a trace that satisfies every rule
above. Release notes and any soundness claim should use this table rather than "authenticated
iteration", which reads as covering the last row.

## Modules touched

| File | Change | Blast radius |
| --- | --- | --- |
| `raster-core/src/input.rs` | `parse_subtree_root` arm `0x0A`; metadata encode helper. No change to `verify_selection_proof` / `step_proves_segment` | **shared parsing — every image id moves** |
| `raster-runtime/src/input.rs` | emit the `0x0A` payload from index metadata | additive |
| `raster-runtime/src/storage.rs`, `backing.rs` | `list_metadata(reference, selector)`; retained backend handle | additive |
| `raster-macros/src/recur.rs` | wrapper traces via metadata instead of `auth_ref_trace` (`:607`) | small, high-value |
| `raster/src/input.rs` | `ListCursor` (indexed only); `resolve_recur_list` deleted; `resolve_recur_list_source` returns a cursor; `run_recur_list*` routed through `select_item`; `build_recur_input` replaced by a materializer returning value + `TraceStorageData` + citations (§4) | core of the change |
| `raster-core/src/draft.rs` | `recur: Option<RecurTileReplay>` on `TileReplayJournal`; `RecurTileReplay`, `RecurPosition`, `RecurControlKind` | **journal encoding — every image id moves** |
| `raster-macros/src/lib.rs` | wrapper fills `recur` — position from the typed `RecurInput`, control from the `ProtocolReturnKind` match, whose existing `Continue \| Break` arm splits (`:1100-1131`) | small |
| `raster-core/src/cfs.rs` | `try_get_recur_iteration_coordinates` (`:412-423`) widened to match `RecurSequence(_)` as well as `RecurTile(_)`, following `expand_recur_entry_coordinates` (`:425-436`) | one line |
| `raster-prover/guests/transition/src/checks/cfs.rs` | completeness rules §5; recur-sequence position read at iteration boundary events | additive |
| `raster-core/src/chunking.rs` | `iteration_chunk_len` and `leading_varint` **deleted** — replaced by `consumed_elements` | deletion |
| `raster-runtime/src/tracing/recorder.rs` | host-side chunk-ordering check (`:778`) subsumed by rule 4 | simplification |
| `raster/src/input.rs` | `chunk_auth_ref` **deleted**; chunked iteration becomes `cursor.select_range` in the driver (§6) | core of the change |
| `raster-macros/src/recur.rs` | the source bound becomes `IntoAuthRef<List<T>>` in both modes (`:592-599`); no synthetic `List<Block<T>>` | small |
| `raster-runtime/src/raster_index.rs` | `Range` descent in `locate`/`select` (`:207`), emitting `ListRange` boundary siblings. Verifier side already exists | moderate |

**Not** changed: the `.rindex` format (metadata is derived from existing Merkle levels), the
proof-step enum, the selector-segment enum, `verify_bound_index_bindings`.

**Image ids do move**, for two independent reasons: `parse_subtree_root` is shared parsing
compiled into every guest, and `TileReplayJournal`'s encoding changes. Revision 1 claimed
otherwise; that was true only while the bound stayed unauthenticated. `paged-bytes` breaks
identity for the same class of reason, so sequencing the three together makes it one migration
rather than three.

Because any later journal field is another such break, §5's journal shape is defined in full
now. `consumed_elements` is needed from day one anyway: chunking is not a later phase (§6).

## Phasing

**All five phases ship together.** They are ordered for implementation and review, not for
release: §6 explains why stopping after any of them leaves chunked recur broken, and phases 3
and 4 remove the two escape hatches it currently relies on.

1. Compact authenticated list metadata: payload, `parse_subtree_root` arm, runtime emitter.
2. Recur tracing consumes metadata instead of resolving the source. **Peak memory moves here.**
3. `ListCursor`: backend decided at open, errors propagate, handle retained. Includes
   `select_range` and the raster-index `Range` descent it needs.
4. Route all three drivers through the cursor (see the matrix in §3).
   `chunk_auth_ref` and `resolve_recur_list` deleted; the wrapper's source bound becomes
   `IntoAuthRef<List<T>>` uniformly; `build_recur_input` replaced per §4, with the item
   binding and its citations recorded on each iteration's step.
5. `TileReplayJournal` gains `recur: Option<RecurTileReplay>`; wrapper fills it;
   recur-tile completeness rules 1–8 and recur-sequence rules S1–S5;
   `chunking::iteration_chunk_len` and `leading_varint` deleted.
6. **Update the skill** — see below. Not optional bookkeeping: the skill is what authors and
   agents read, and it currently frames the raster-source requirement as a performance rule.
`paged-bytes` needs all five: 1–4 to sweep at all, 5 to claim the sweep covered the region.

## SKILL.md updates

**Already landed, as forward-looking guidance.** `.claude/skills/raster/SKILL.md` §7 now
carries an `input` bullet stating that a recur source must come from a raster-encoded input,
with the `examples/hello-tiles/bin/gen_input.rs` pattern (postcard input for scalar `select!`s,
raster input for sweeps) and the note that internally stored values always carry an index. §9's
symptom table gained the matching row. This is accurate today — it is a performance rule — and
lets existing programs, `raster-chain-inference` in particular, be aligned **before** the
change lands rather than being broken by it.

**Do not forget at implementation.** Phase 6 is a wording flip, not an addition:

- §7's `input` bullet: "Today this is a performance rule; when
  `docs/proposals/lazy-list-recur.md` lands it becomes an error at the `call_recur!` site" →
  drop the hedge and state the requirement, with the exact `open` error text from §3.
- §9's symptom row: the symptom stops being "peaks at whole-collection memory" and becomes the
  error message, since the failure mode changes from slow to refused.
- §7's decision-tree row for `chunk = N` — the mechanism becomes driver-level range selection
  (§6), which does not change the authoring spelling but does change what
  `references/recur.md` should say about how a chunk is proved.
- `references/data-and-io.md`: recur sources in the fixture-format section, and the
  `encoding = "raster"` + `index_path` declaration as a requirement rather than a tuning knob.

The skill is the enforcement surface for everything this proposal cannot express in types.
Leaving it describing the old behaviour is how a rule becomes folklore.

## Verification

- **Peak-RSS benchmark, before and after**, on a recur over a large storage-backed list —
  the acceptance criterion, since the claim is a change of asymptotic class.
- A materialization counter proving exactly one element is resolved per iteration.
- **Payload-kind pinning:** a recur-source binding carrying a `0x02` whole-list payload is
  rejected, even though that payload folds to the same root and its length is equally
  authenticated. The failure is refusal, not fallback-to-parsing — the test asserts the audit
  never walks the list. Conversely a `0x0A` payload where a range proof is expected fails in
  `parse_list_child_roots`, confirming each consumer pins its own kind.
- Metadata: round-trip at `len` = 0, 1, 2, odd and even element counts (the Merkle
  duplication path); a forged `len` fails to fold; a forged `elements_root` fails to fold; the
  empty-list sentinel verifies.
- **The forged `len = 0` sweep is rejected** — the case that motivates §1 and §5.
- Completeness, one test per rule: non-zero `iteration_index` first; a gap in indices; a
  `declared_iterations` disagreeing with `⌈L / C⌉`; `consumed_elements` of zero or overrunning
  `L`; a short *non-terminal* chunk (`4,1,4,1` at `C = 4, L = 10`, rejected at iteration 1 by
  rule 4's equation); a short *final* chunk that is legal (`4,4,2`, accepted); a terminal
  `Continue` after correctly-shaped but too few iterations (`4,4` at `L = 10`, which passes
  rules 2, 3 and 4 and is caught only by rule 5); a `Break` followed by another iteration; zero
  iterations against `L > 0` and against `L == 0`.
- **The undersized `Break`**, which the exemption in an earlier draft admitted: `L = 100`,
  `C = 4`, one iteration consuming 1 element and returning `Break`. It must be rejected by rule
  4 even though rules 3, 6, 7 and 8 all pass and the range selection is honest. Pair it with the
  legal case — a `Break` on a full chunk mid-source (`consumed == 4`, `covered_end == 4 < 100`)
  — so the tests show rule 4 constrains the size while rule 6 constrains only what follows.
- Rule 8, one test per disagreement: `ListRange.len ≠ L`; `ListRange.start` ahead of or behind
  the running total; payload element count `≠ consumed_elements`. Each must be rejected even
  though the journal alone is self-consistent — that is the point of the cross-check.
- **The prefix/terminal split, tested as a pair:** a legitimate mid-sweep `Break` with
  `covered_end < L` is **accepted**, in every recur mode that can produce one, while a terminal
  `Continue` with the identical coverage is **rejected**. These two must be in the same test
  file — the earlier draft's "covers `[0, L)`" rule made the first of them impossible, and only
  running them together shows the rules admit one and not the other.
- Recur-sequence rules S1–S5, one test each: a missing End event (S2); a first index other
  than zero and a gap in indices (S3); fewer and more iterations than `L`, plus `L == 0` with
  one iteration and `L > 0` with none (S4); and a `Break` in a `call_recur!` nested inside an
  iteration, which must terminate only the inner site while the outer sweep still runs to `L`
  (S5).
- No recur-sequence test relies on a recur-tile rule number — the two sets are checked by
  separate code paths and should be exercised by separate fixtures.
- Journal population, exhaustively: an ordinary tile emits `recur: None`; every recur tile
  emits `Some`, including modes whose return type carries no `RecurControl` — those must carry
  an explicit `Continue`, not an absence; an ordinary tile in a recur sequence's own body emits
  `None`.
- **Attribution is by site, not by subtree:** a `call_recur!` nested inside a recur-sequence
  iteration emits `Some` on its tiles, attributed to the nested site. The outer site's rules
  must read only journals belonging to it — the regression test for the containment trap, and
  the case that makes "nothing inside a recur sequence emits `Some`" wrong as a blanket
  statement.
- A recur tile in a no-early-exit mode is checked by rule 6 like any other, rather than
  skipped for want of a control value — the regression test for removing the default.
- A `Break` inside a `call_recur!` nested in a recur-sequence iteration terminates **only** the
  inner loop; the outer sweep must still run to `L`. This is the containment-attribution trap,
  and it is the regression test for it.
- Recur-sequence position read at iteration boundaries agrees with what the equivalent
  recur-tile sweep records — including the negative that splitting an *inner* step's
  coordinates yields the step index, not the iteration index.
- `try_get_recur_iteration_coordinates` resolves a recur-sequence site after the widening, and
  still resolves recur-tile sites unchanged.
- Chunked recur validates from `consumed_elements` with `iteration_chunk_len` removed —
  including the case its leading-varint assumption would have mis-read.
- **Chunked recur, end to end on the range path**: `examples/hello-tiles` (`chunk = 2`,
  `main.rs:125`) produces a bit-identical result before and after, with `index`, `len`,
  `is_first` and `is_last` unchanged inside the step — the regression test for "the authoring
  surface does not change".
- `select_range` proofs: a chunk at the start, in the middle, at the end, a short final chunk,
  `chunk` larger than `L`, and `L` an exact multiple of `chunk`; each folds to the committed
  list root through `ListRange`.
- `RasterIndex::select` emits `ListRange` boundary siblings that `fold_list_range` accepts —
  the two halves have never met before, since `:207` rejected range descent outright.
- A range selection that overruns `L` is rejected by the index, not by the verifier.
- Item provenance: each iteration's step records the item's `TraceStorageData` — selector
  `source[i]`, commitment, selection — and the tile's input bytes are unchanged from today,
  proving the carrier stayed host-side and the replay ABI did not move.
- Citations survive materialization: `select!(List<Row>, tables[table_id])` swept by
  `call_recur!` records, on every iteration, the storage binding that authorized `table_id`.
  The negative is the one that matters — dropping `index_bindings` must fail with
  `BoundIndexViolation::MissingSource` rather than passing quietly, since that is the symptom
  a lost citation actually produces.
- The `[A, B, C]` substitution, once the release gate lands: a trace whose item proof selects
  `B` at `source[1]` while the replay journal's input decodes to `C` is rejected. Until then it
  is a known-accepted case and should be marked as such rather than left absent — a missing
  test reads as coverage.
- Error taxonomy: a malformed index propagates rather than falling back; an out-of-range
  selector propagates. There is no fallback path to reach, so these are checked as plain
  errors rather than as a discrimination between failure and degradation.
- A `call_recur!` over an external input declared `encoding = "postcard"` fails at `open` with
  the re-encode message, and the same data re-imported as `encoding = "raster"` sweeps
  correctly — the migration path, tested as a pair.
- Every internal source opens `Indexed`: a tile output, a finalized draft, and a `store_value`
  result each sweep without error, confirming `store_value_at_coordinates`'s unconditional
  raster payload (`storage.rs:1004`).
- Existing recur suites green: element, state, output, chunked, recur sequences, early `Break`,
  empty input.

## Performance

Structural, not estimated.

| scenario | today | after |
| --- | --- | --- |
| 100k rows × 1 KiB | ~300 MB held for the loop | index + one element |
| 1 GiB page list (`paged-bytes`) | ~3 GiB before the first tile | index (~460 KB) + one page |
| index parses per sweep, owned objects | one per item | one per cursor |
| recur source trace payload | whole list | 41 bytes (9 when empty) |
| chunked recur | whole list + a regrouped copy | one chunk, by range selection |

Peak is `O(index + element + witness)`, not `O(element)` — the parsed index stays resident by
design. Cycle counts and proof sizes are otherwise unaffected; this changes what the host
holds, plus one 41-byte selection per recur site (9 for an empty source).

## Uncertainties for review

1. ~~How the guest recognizes a replay-proven `Break`.~~ **Closed.** Typed fields in the replay
   journal, filled by the generated wrapper (§5) — neither output-byte parsing nor a
   normalized recur ABI. The residual question about recur sequences is closed too, in the
   strongest way available: they cannot terminate early at all (`recur.rs:317`), so there is
   no termination fact to carry and no attribution to get wrong.
2. ~~Should metadata have its own selector segment?~~ **Resolved: no** (§1). A selector segment
   names a *descent* — `Field`, `Index`, `Range`. Metadata is a different view of the same
   node, not a step down the tree, so a `SelectorSegment::Metadata` would be categorically
   wrong as well as breaking the one-step-per-segment rule. The consumer pins the payload kind
   instead. `SelectionPayloadKind` on the commitment remains available if trace readability
   ever justifies it, but it is derivable from byte 0 of the payload, so a trace *viewer* can
   surface it with no format change — the cheaper place to solve a readability problem.
3. ~~Keep `Materialized` at all?~~ **Resolved: removed** (§3), and migration scope surveyed —
   see below. Nothing outstanding.
4. ~~Does completeness block `paged-bytes`' release?~~ **Resolved: it gates the claim, not the
   code.** Completeness blocks releasing any *authenticated sweep claim* — "this program
   processed the whole artifact". It does not block the format work or random access: a
   `select!` at a computed page index is authenticated by its own selection proof and needs no
   sweep rule. `paged-bytes` can therefore ship its format and addressing with completeness
   landing in the same release, and only the sweep-coverage claim waits on it.
5. ~~Payload tag allocation.~~ **Resolved: `0x0A` metadata, `0x0B` bytes page**, recorded
   centrally in `parse_subtree_root`'s doc comment rather than in either proposal (§1).
6. ~~Should the metadata form be used outside recur?~~ **Resolved: internal to recur in v1.**
   The payload stays a runtime/audit mechanism with no authoring surface — no `select!` spelling
   produces one. Widening it later is additive; narrowing it after programs depend on it is not.
7. ~~An interim hash-in-place step.~~ **Removed.** It was proposed as a way to get memory
   relief before the format change, and neither half of that holds. The phases now ship
   atomically (§Phasing), so there is no window for an interim; and hashing the payload in
   place only bounds *host* memory — `selected_hash` would still be computed over the whole
   list, and the whole-list payload would still be the recorded witness the transition guest
   receives. It trades a host allocation for an unchanged proving witness, which is the more
   expensive half.
