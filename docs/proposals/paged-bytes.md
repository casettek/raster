# Proposal: `paged-bytes` — `Bytes`/`BytesPage`, byte data that is addressed, never materialized

Status: implemented 2026-08-14 (revision 3), **with defects — see §12**. Gate 2 (chunked-sweep `ListRange` cross-check) and Gate 3 (selection↔replay bind) remain open, as do `BoundRange`, a framework `page_of` tile, lazy-list-recur rule 8, and window-seed-reconstruction.

Related:
- [`bounded-collections.md`](./bounded-collections.md) — this is the same `List`/`Block`
  split one granularity down. `Bytes` is to `List<T>` what `BytesPage` is to `Block<T>`.
- [`dynamic-index-selection.md`](./dynamic-index-selection.md) — a computed page index is an
  ordinary `BoundIndex` citation, used exactly as it ships today. No new provenance
  mechanism and no new variant.
- [`program-identity.md`](./program-identity.md) — page size is declared in the Rust type;
  committing it requires adding a schema hash to `InterfaceDecl` (§2.1).
- [`lazy-list-recur.md`](./lazy-list-recur.md) — **blocking dependency, ships first.** Two
  things come from there. (a) `call_recur!` materializes its whole source list before the
  first iteration — in tracing (`raster-macros/src/recur.rs:607`) as well as in the runners
  — so until that is fixed, sweeping a region costs `O(artifact)` host memory no matter how
  well the format works and every figure in §8 is unreachable. (b) The loop bound is
  currently index-trusted, so a sweep cannot be claimed to cover a whole region until its
  authenticated list metadata and completeness auditing land.

Supersedes the withdrawn `BoundedBlob<MAX_BYTES>` draft (§6).

**Revision 2 changes.** Revision 1 proposed a standalone `SelectionProofStep::Bytes`
wrapper step and a dedicated `RasterNodeKind::Bytes`. That proof layout **cannot verify**:
`verify_selection_proof` requires exactly one proof step per selector segment
(`raster-core/src/input.rs:723-733`), and a wrapper step has no segment to pair with.
Revision 2 fixed this by giving `Bytes` a struct shape, which removes the wrapper, the new
node kind, the new proof step, and the bespoke recur driver together. It also moved page
size from the artifact into the Rust type, and added byte-offset addressing at the API.

**Revision 3 changes.** Revision 2's `IndexTransform::PageOf` **cannot be audited**: the
arm needs `byte_offset` to check containment, and the verifier only ever sees the cited
binding's hash — today's encode-and-compare works precisely because it never decodes a
cited value (`trace.rs:157-170`). Revision 3 drops the transform and does byte→page
conversion in a tile (§1.5), which is where arithmetic belongs and which needs no audit
change at all. It also relocates the geometry checks to where the proof witnesses are in
scope (§3.2), and records that committing the declared page size to program identity
requires one new field on `InterfaceDecl` (§2.1). Finally it settles the sweep spelling: an
`IntoAuthRef` adapter would rewrite a selector invisibly to the flow resolver, so sweeps
select `.pages` explicitly (§1.3).

## Problem

Raster cannot carry raw bytes at all. Both serde bridges reject them outright —
`raster-runtime/src/input.rs:173` and `raster-core/src/draft.rs:202` both return
"raw bytes are not supported". The two workarounds are each unusable at the scale that
motivates the feature (an LLM weight artifact, 10⁸–10¹⁰ bytes):

1. **`List<u8>`.** One index node and one Merkle leaf per byte. A 1 GiB region costs
   ~2.1 × 10⁹ hashes ≈ 68 GB of `.rindex` before any data is read. Not slow — impossible.
2. **Hex in a `String`.** 2× expansion on disk, in the replay input, and in the input
   commitment, plus a nibble decoder burning zkVM cycles in every tile.

Underneath both is a structural gap: **byte data has a natural addressing granularity and
Raster has no way to express it.** A weight file is not a list of scalars and not one
opaque value; it is a sequence of fixed-size runs addressed by byte offset. Authors are
forced to encode it either one byte at a time or all at once.

## Goal

Give byte data the treatment `bounded-collections.md` gave element collections: split
*addressability* from *materializability*, and make the unbounded case unrepresentable.

```text
List<T>  :  Block<T>   ::   Bytes  :  BytesPage
```

- **`Bytes`** — a paged byte region. `Selectable`, **not** `Materializable`. It lives in
  committed storage, is addressed by byte offset, and never crosses a tile boundary whole.
- **`BytesPage`** — one page. `Materializable` and terminal. What a tile receives.

Two properties the design is built around:

- **Page size is declared in the Rust type.** It is interface, not payload — checkable at
  load, present in the schema, and known to the macro at expansion time.
- **The consumption unit is the commitment unit.** A page is both the Merkle leaf and the
  replay unit. This is what removes every piece of misalignment machinery.

## 1. User-facing API

### 1.1 Declaring the data

```rust
// src/input.rs — the shared no_std types crate
use raster::prelude::*;

#[derive(Serialize, Deserialize, Selectable)]
pub struct ModelFile {
    pub input_width: u32,
    pub row_stride: u32,                  // 16 KiB — 4096 i32 per row

    /// Where each layer's weights start, in bytes, within `weights`.
    pub layers: List<LayerEntry>,

    /// 256 KiB pages = 16 rows per page. A multiple of `row_stride`, so no row
    /// ever straddles a page boundary.
    #[page_size = 262_144]
    pub weights: Bytes<262_144>,
}

#[derive(Serialize, Deserialize, Selectable)]
pub struct LayerEntry {
    pub layer_id: u32,
    pub byte_offset: u64,                 // global offset into `weights`
    pub byte_len: u64,
}
```

`#[page_size = n]` is the only new authoring surface. It does three jobs:

1. the derive emits `pub const WEIGHTS_PAGE_SIZE: u64 = 262_144;` on `ModelFile`, so the
   fixture generator cannot drift from the declaration;
2. the artifact is checked against it at load — a file paged at 64 KiB is rejected against
   this type before anything is mapped;
3. the macro knows it at expansion time, which makes literal range alignment a
   **compile-time** diagnostic (§1.4c) rather than a runtime size surprise.

`ModelFile` is `Selectable` but not `Materializable` — it has `List` fields — so passing
the whole model into a tile is the existing compile error with the existing diagnostic.
`Bytes` itself is never `Materializable` for the same structural reason (§2.1).

### 1.2 Writing the artifact

```rust
// fixtures/src/main.rs — host-side, std
use raster_runtime::input::write_raster_files;

fn main() -> anyhow::Result<()> {
    let raw: Vec<u8> = load_weights("model.safetensors")?;
    let layers = layer_table_for(&raw);              // byte offsets, page-aligned

    let model = ModelFile {
        input_width: 4096,
        row_stride:  4096 * 4,
        layers:      List::from(layers),
        weights:     Bytes::<{ ModelFile::WEIGHTS_PAGE_SIZE }>::paged(raw)?,
    };

    let commitment = write_raster_files(
        &model,
        Path::new("model.rastered"),
        Path::new("model.rindex"),
    )?;
    println!("model commitment: {commitment}");
    Ok(())
}
```

`Bytes::paged` is the only constructor — no `From<Vec<u8>>`, because a region without a
page size has no addressing scheme. It splits into `⌈len/page_size⌉` pages, the last one
short, and errors on `page_size == 0`.

Write layer offsets **page-aligned** where you can. It costs nothing at import and is the
difference between §1.5 working as written and needing a multi-page selection.

Fixtures are otherwise unchanged:

```jsonc
// input.json  (private)
{ "model": { "path": "model.rastered", "index_path": "model.rindex",
             "load_preference": "mmap" } }
// input_manifest.json  (public)
{ "model": "9f2c…" }
```

### 1.3 Sweeping — recur over pages

```rust
#[sequence]
fn main(model: ModelFile) -> Accum {
    let pages = select!(List<BytesPage>, model.weights.pages);
    call_recur!(
        tile  = accumulate_page,
        input = pages,
        state = Accum::default(),
    )
}
```

Because `Bytes` is a struct whose third field is `List<BytesPage>` (§2.1), the **driver** is
the existing list recur, and `chunk = 4` works unchanged if per-call overhead ever dominates.
Iteration count is `⌈byte_len/page_size⌉`, derived from committed fields.

**The `.pages` hop is written explicitly**, and that is a deliberate choice rather than an
omission. The generated wrapper requires `IntoAuthRef<List<#item_ty>>`
(`raster-macros/src/recur.rs:592-599`), and `AuthRef<Bytes>` only satisfies
`IntoAuthRef<Bytes>` (`raster/src/input.rs:1464`) — containing a `List<BytesPage>` field does
not make it one. So something has to bridge them, and there are two ways.

**Rejected: a trait adapter.** `impl IntoAuthRef<List<BytesPage>> for AuthRef<Bytes>` would
let `input = weights` work by appending `Field("pages")` to the selector inside the impl. It
is coherent (neither `impl<T: Serialize> IntoAuthRef<T> for T` at `:1437` nor
`impl<Current> IntoAuthRef<Current> for AuthRef<Current>` at `:1464` overlaps it) and it
would work in the common case. But the appended segment is invisible to the flow resolver, so
the recorded selection path is one segment longer than what the CFS attributed. Direct
bindings tolerate that — `checks/cfs.rs:339-357` asserts only the binding *kind* — but
`InputBinding::SequenceScope` routes through `assert_same_source` (`cfs.rs:38-56`), which
compares full `StorageData` including `selection.path`. A region passed across a `call_seq!`
boundary and swept in the callee would fail audit with "Storage sequence scope input does not
match consumer binding".

`dynamic-index-selection.md` records this lesson from `into_ref!` in as many words: *a
sequence-body form the flow resolver cannot name is unsound by default*, and new surface
belongs in the macro grammar rather than on an inherent impl. A trait conversion that
silently rewrites a selector is exactly such a form. Two context-dependent spellings, one of
which fails only across a boundary, is not worth one saved token.

The explicit hop is resolver-visible, works everywhere, and needs no new plumbing at all:

```rust
let pages = select!(List<BytesPage>, model.weights.pages);
call_recur!(tile = accumulate_page, input = pages, state = Accum::default());
```

If the one-line form is wanted later, it should be a resolver-recognized macro —
`pages!(weights)`, registered in the grammar the way `into_ref!` was — not a trait
conversion. That is deferred; see the open questions.

### 1.4 Selecting

```rust
// whole region as a reference — nothing materialized
let weights = select!(Bytes, model.clone().weights);

// one page, addressed by BYTE offset (§1.5)
let page    = select!(BytesPage, model.clone().weights[start]);

// a literal byte range — yields whole covering pages
let window  = select!(Block<BytesPage>, model.weights[524_288..1_048_576]);
```

**(c) Range semantics.** A range returns the whole pages covering `[a, b)`:

```text
first = a / P        last = (b - 1) / P        count = last - first + 1
```

The bytes you asked for are somewhere inside; the tile slices with `a - page.offset()`.
Page count therefore depends on alignment, not only on length:

| request (`P` = 256 KiB) | pages |
| --- | --- |
| `[0 .. 262_144)` | 1 |
| `[1 .. 262_145)` | 2 |
| `[0 .. 1_048_576)` | 4 |
| `[1 .. 1_048_577)` | 5 |

So a request of length `L` yields `⌈L/P⌉` or `⌈L/P⌉ + 1` pages. Three consequences:

1. **Budget one page of headroom.** A tile sized for 4 pages will sometimes receive 5.
2. **Never drive a recur with byte ranges.** `chunking.rs:83-91` requires every chunk to
   be exactly the declared size except the last; an alternating 4/5 page count fails
   `check_previous_chunk_was_full` on the first short iteration. The rule:
   **ranges are for random access, recur is for sweeps.** Sweeps use `chunk = m` over
   pages, which is aligned by construction and never wobbles.
3. Tile input size stops being a static property of the call site.

Because `page_size` is in the type, the macro can check literal bounds at expansion:

```rust
select!(Block<BytesPage>, model.weights[524_288..1_048_576]);  // aligned — always 2 pages
select!(Block<BytesPage>, model.weights[524_289..1_048_577]);  // diagnostic: unaligned, 3
```

**The emitted selector is in page units, not bytes.** `model.weights[524_288..1_048_576]`
expands to `Range { start: 2, end: 4 }`. This is load-bearing, not cosmetic: the
`(ListRange, Range)` arm of `step_proves_segment` (`input.rs:614`) compares the segment's
bounds against the step's, so a byte-unit segment could never match a page-unit proof step —
and making the verifier divide is exactly the change §2.1 exists to avoid. Converting at
expansion is also why computed range bounds cannot work: the macro has no value to convert.
The consequence to know when debugging: `cfs.json`, traces, and `SelectionCommitment.path`
show page indices while the source shows byte offsets.

**Computed range bounds are out of scope for v1.** `SelectorSegment::Range` has no
citation channel — SKILL §5 says it outright ("Ranges keep literal bounds") — so
`weights[start..start + n]` needs a `BoundRange` with its own entry in `bound_indexes`,
its own encode-and-compare, and its own place in the cycle graph. A computed offset with a
literal length covers the access patterns we have; see open question 3.

### 1.5 Addressing by byte offset

A byte offset is not a page index, and the conversion between them is arithmetic — so it
happens in a tile, where arithmetic belongs (SKILL §4), not inside the selector:

```rust
#[sequence]
fn main(model: ModelFile, request: LayerRequest) -> LayerOut {
    let layer_id = select!(u32, request.layer_id);
    let layers   = select!(List<LayerEntry>, model.clone().layers);
    let entry    = select!(LayerEntry, layers[layer_id]);

    let start = select!(u64, entry.clone().byte_offset);
    let len   = select!(u64, entry.byte_len);

    // The region's own committed page size — a selectable field of the region struct
    // (§2.1), not a literal, so the index is bound to the artifact it addresses.
    let page_size = select!(u64, model.clone().weights.page_size);
    let start_ix  = clone!(start);
    let page_idx  = call!(page_of, start_ix, page_size);     // replayed and proven

    let page = select!(BytesPage, model.weights[page_idx]);  // ordinary BoundIndex

    call!(apply_layer, page, start, len)
}

#[tile(description = "Page containing a byte offset", estimated_cycles = 200)]
pub fn page_of(byte_offset: u64, page_size: u64) -> u64 {
    byte_offset / page_size
}
```

What that emits:

```text
path:   [ Field("weights"),
          Field("pages"),
          BoundIndex { index: 18, source: "page_idx", width: U64 } ]

steps:  [ Struct, Struct, List ]                       // all pre-existing
```

Every piece already exists. `page_of` is an ordinary tile whose output is a committed
storage value; `page_idx` is an ordinary citation discharged by
`verify_bound_index_bindings` (`trace.rs:141-181`) with **no new arm, no transform, no new
width rule, and no new entry in the cycle graph**. Paths stay canonical — one page, one
spelling — because the cited value *is* the page index.

**Why byte→page is not resolved inside the selector.** A `PageOf` transform on `BoundIndex`
would have to establish `index × page_size ≤ byte_offset < (index + 1) × page_size`, and the
verifier never learns `byte_offset` — it holds only the cited binding's `selected_hash`.
Today's mechanism is sound precisely because it never decodes a cited value: it re-encodes
the *claimed* index and compares one hash (`trace.rs:157-170`). A containment check needs the
value itself, which leaves two bad options — carry it in the segment, which makes paths
non-canonical (two offsets in one page, two different paths), or decode it in the verifier,
adding to guest audit code exactly the integer decoder `IndexWidth`'s doc comment
(`input.rs:64-79`) exists to keep out. A tile costs three lines and a few hundred cycles per
random access, and nothing at all on a sweep, which never addresses by offset.

**The requested offset is not part of the page value.** A page's Merkle leaf must be
stable regardless of which byte was asked for; if the selected payload carried the request,
`parse_subtree_root(selected_bytes)` would hash it and produce a root that does not match
the leaf. The offset is already an authorized binding — the one the index was computed
from — so it travels as its own tile argument. This keeps
`local = start - page.offset()` an honest authorized computation.

### 1.6 Tile usage

```rust
#[tile(kind = recur, description = "Accumulate one page of weights",
       estimated_cycles = 1_200_000)]
pub fn accumulate_page(input: RecurInput<BytesPage>, state: Accum) -> Accum {
    let page = input.into_value();
    let bytes = page.as_slice();
    assert!(bytes.len() % 4 == 0, "page is not i32-aligned");

    let mut state = state;
    for word in bytes.chunks_exact(4) {
        state.sum += i64::from(i32::from_le_bytes(word.try_into().unwrap()));
    }
    state
}

#[tile(description = "Apply one layer's weights", estimated_cycles = 900_000)]
pub fn apply_layer(page: BytesPage, start: u64, len: u64) -> LayerOut {
    // `page.offset()` is committed; `start` is the authorized byte offset that
    // selected it. The difference is the position inside the page.
    let local = (start - page.offset()) as usize;
    let end   = local + len as usize;
    assert!(end <= page.len(), "layer spans more than one page");

    let mut out = LayerOut::default();
    for word in page.as_slice()[local..end].chunks_exact(4) {
        out.absorb(i32::from_le_bytes(word.try_into().unwrap()));
    }
    out
}

#[tile(description = "Scan a window of pages")]
pub fn scan_window(pages: Block<BytesPage>) -> Hit {
    let mut hit = Hit::default();
    for page in pages.iter() {
        let base = page.offset();
        for (k, word) in page.as_slice().chunks_exact(4).enumerate() {
            hit.consider(base + (k as u64) * 4,
                         i32::from_le_bytes(word.try_into().unwrap()));
        }
    }
    hit
}
```

The tile-side contract:

| a tile receiving a `BytesPage` | |
| --- | --- |
| `page.offset()` | global byte offset of page start — committed, audited `== index × page_size` |
| `page.index()` | page number — committed |
| `page.as_slice()` | the bytes; `page_size` long except on the last page |
| `page.len()` | actual length — assert element alignment, do not assume |
| the offset you *asked for* | not on the page; passed alongside as its own authorized binding |
| mutation | none — no `DerefMut`, no public constructor |
| `Debug` | `BytesPage { index, offset, len }`, never the payload |

**Pagination is visible in tiles no matter how the sequence is written.** A tile receives
a page and must know where in it to look. Hiding pagination in the selector changes the
sequence's spelling, not the tile's arithmetic — which is why `start` is passed explicitly
rather than smuggled into the page value.

## 2. Artifact format

### 2.1 `Bytes` is a struct

This is the change that makes the whole feature cheap. `Bytes` is not a new node kind; it
expands to a struct whose third field is a page list:

```text
Bytes  ≡  struct "$raster::Bytes<262144>" {
              byte_len:  u64,
              page_size: u64,
              pages:     List<BytesPage>,          // 0x09 list handle
          }
```

```text
payload:  0x01 struct
            ├── 0x00 leaf   byte_len
            ├── 0x00 leaf   page_size
            └── 0x09 handle [root:32][len:8][inner_len:8]
                  └── 0x02 list
                        ├── 0x0B page  index=0 offset=0       len=262144 ‖ bytes
                        ├── 0x0B page  index=1 offset=262144  len=262144 ‖ bytes
                        └── …
```

Consequences, each of which deletes work revision 1 proposed:

- **No new proof step.** Selecting page 18 is `[Struct, Struct, List]`, all implemented.
  `verify_selection_proof` and `step_proves_segment` are untouched — the highest-consequence
  code in the system does not change.
- **No new index node kind.** `RasterNodeKind::Bytes` disappears; it is `Struct`, `Leaf`,
  `List`.
- **No new recur driver.** `weights.pages` is a `List<BytesPage>`; recur over it is the
  existing driver, and `chunk = m` is the existing chunking.
- **Non-materializability is free.** A struct with a `List` field is `Selectable` only,
  under the rule that already exists.
- **Geometry is bound globally.** `byte_len` and `page_size` are ordinary leaf fields, so
  `struct_commitments_root` ties them to the page-list root. A page's proof carries their
  roots as `siblings` in the second `Struct` step (`SelectionProofStep::Struct` records
  `field_names` and `siblings`, `input.rs:237-249`), which is exactly where the audit reads
  them.
- **Root recomputation stays O(1)** in the guest: two leaves plus a `0x09` handle that is
  skipped without hashing (`input.rs:441-452`).

The declared page size reaches the schema through the canonical type name
(`"$raster::Bytes<262144>"`), emitted by the `Selectable` derive from the field attribute
and following the `LIST_HANDLE_NEWTYPE_NAME` precedent at `collections.rs:85-94`. The same
sentinel technique is what lets `TreeValueSerializer` recognize a `BytesPage` and emit tag
`0x0B` instead of an ordinary struct — a `BYTES_PAGE_NEWTYPE_NAME` constant beside the list
one. Without it the page tag is never produced and the format silently does not exist.

**That name is not in program identity today, and making it so takes one field.**
`struct_commitments_root` hashes `b"struct"` plus per-field `(name, child_root)` and no type
name (`input.rs:323-334`), so the string does not reach the structural root. `InterfaceDecl`
carries only `type_path` and `encoding` (`program.rs:39-47`), so it does not reach
`program_commitment` either — and because `page_size` is an artifact field rather than a
const in tile code, no image id moves when the declaration changes. Add
`schema_hash: [u8; 32]` to `InterfaceDecl`, derived from `Schema::schema_hash()`
(`input.rs:206-213`), and the property holds by construction: `#[page_size]` becomes an
identity change with no new schema variant. Without that field the declaration is real and
enforced at load, but uncommitted.

### 2.2 Page payload — tag `0x0B`

```text
0x0B ‖ index:u64 ‖ offset:u64 ‖ len:u64 ‖ bytes
root = H(b"bytes-page" ‖ index ‖ offset ‖ len ‖ bytes)
```

`index` and `offset` are redundant with the proof position and with
`index × page_size` — deliberately. They make a page self-describing to the tile that
receives it, and the redundancy is *audited* (§3.1) rather than trusted.

A distinct tag and hash domain are required, not stylistic: leaf roots are
`H("leaf" ‖ leaf_bytes)` with no type discriminator (`input.rs:1413`), so a page folded
into the leaf domain could collide with a `String` holding the same bytes.

**Framing overhead** is 8 bytes (list child-length prefix) + 25 bytes (tag, index, offset,
len) per page = 33 bytes, or 0.013% of a 1 GiB region at 256 KiB pages. The `.rastered`
file is therefore not a byte-identical copy of the source weight file; importing is a real
conversion pass. Deliberate non-goal — the withdrawn `RawBlob` encoding was the
alternative and bought nothing else.

### 2.3 Index

No new node kind. The region contributes one `Struct` node, two `Leaf` nodes, and one
`List` node with `⌈byte_len/page_size⌉` page children and their Merkle levels. Index cost
is `2⌈byte_len/page_size⌉` hashes plus one node record per page — flat in `byte_len` for a
fixed page size, halving as page size doubles.

Bump the index magic to `rindex03` for the new payload tag.

## 3. Audit

### 3.1 Region and page shape

Discharged in the transition guest against committed values only:

1. **Schema conformance.** The committed `page_size` field equals the type's declared
   `#[page_size = n]`. Checked at load on the host, and pinned in identity via the
   canonical type name (§2.1).
2. **Partition.** `pages.len() == ⌈byte_len / page_size⌉`, with both values checked by
   encode-and-compare against the leaf roots carried as `siblings` in the enclosing
   `Struct` step (§3.2 for where this runs).
3. **Page shape.** For page `i`: `offset == i × page_size` and
   `len == min(page_size, byte_len − offset)`. This is what makes rule 2 a partition
   rather than a count — without it an artifact could ship overlapping or gappy pages
   while every individual page still verified.
4. **Sweep conformance.** Recur over `pages` uses the existing list-recur and `chunking.rs`
   rules unchanged.

### 3.2 Where the geometry checks live

Rules 2 and 3 compare `byte_len` and `page_size` against leaf roots, and those roots arrive
as `siblings` in the enclosing `Struct` step of a page's selection proof. So the check
belongs in `checks/store.rs`, beside `verify_selection_witness` (`store.rs:191-201`), which
is the one place the witness is in scope.

It cannot live in `verify_bound_index_bindings`, where revision 2 put it:
that function takes `&StorageInput`, whose `StorageInputData.selection` is a
`SelectionCommitment` (`trace.rs:41`) — path, source root hash, payload hash, length. No
proof steps, therefore no sibling roots, therefore nothing to compare against.

Byte-offset addressing adds nothing to the audit at all. The page index is produced by a
tile (§1.5), so its citation is an ordinary `BoundIndex` and `verify_bound_index_bindings`
is untouched — one scan, one audit function, one cycle graph, one width rule, exactly as
`dynamic-index-selection.md` ships them.

**Why no dedicated `BytesIndex` segment either.** `verify_bound_index_bindings` finds its
work by scanning paths for `BoundIndex` (`trace.rs:143`, and again at `:223` for the cycle
graph). A new segment kind carrying a citation would be invisible to both — it would
compile, run, and pass the audit with an entirely unchecked index. That is the exact failure
`dynamic-index-selection.md` records for `into_ref!`: a form the resolver could not name
fell through to `InputSource::Inline`, the permissive fallback. **New surface the audit
cannot name is unsound by default** — which is the same argument that rules out a new
*transform* the audit would have to learn to interpret.

### 3.3 Release gate: bind storage selection to replay values

Storage selection proofs and tile replay inputs are still verified independently:
`checks/store.rs:200` proves *some* bytes were selected from the committed artifact;
`checks/io.rs:109` proves the tile ran on *some* recorded bytes. Nothing asserts they are
the same value, because the guest never decodes the postcard input.

This is a **pre-existing gap affecting every Rastered type**, and the fix is generic: tile
guests commit the structural root of each decoded input and output in `TileReplayJournal`,
and the transition audit requires every storage-bound input root to equal its verified
selection root. It may ship as a companion PR, but `Bytes` must not be described as
end-to-end authorization-sound until it lands.

## 4. Modules touched

| Crate / file | Change | Blast radius |
| --- | --- | --- |
| `raster-core/src/collections.rs` | `Bytes` (struct-shaped, private fields, `paged` ctor), `BytesPage`; `Materializable for BytesPage` | additive |
| `raster-core/src/input.rs` | `parse_subtree_root` arm `0x0B`; `BYTES_PAGE_NEWTYPE_NAME` sentinel. **No change to `verify_selection_proof` or `step_proves_segment`** — which holds only because the macro emits page units (§1.4c) | small, high-value |
| `raster-core/src/trace.rs` | unchanged — a tile-produced page index is an ordinary citation (§3.2) | none |
| `raster-core/src/draft.rs` | `serialize_bytes` arm (errors today at `:202`); `DraftValue::BytesPage`; finalize parity | additive |
| `raster-core/src/chunking.rs` | reused unchanged | none |
| `raster-core/src/error.rs` | `PageShape`, `PageSizeMismatch`, `PageSizeZero` variants | additive |
| `raster-core/src/program.rs` | `schema_hash: [u8; 32]` on `InterfaceDecl` — without it the declared page size reaches no commitment (§2.1) | identity change |
| `raster-runtime/src/input.rs` | `TreeValue::BytesPage`; `serialize_bytes` (`:173`); `deserialize_bytes`/`byte_buf` (remove from the forward list at `:937`); `assemble_subtree` arm; `parse_bytes_page`; page-shape check in `selected_payload_from_raster_location` (`:1831`) | moderate |
| `raster-runtime/src/raster_index.rs` | `0x0B` leaf handling; magic → `rindex03`. **No new `RasterNodeKind`.** | format bump |
| `raster-runtime/src/entry_arguments.rs` | `ReferencedSourceKind::Raster { schema }` — schema is dropped for raster sources today (`:118-124`); needed for the load-time page-size conformance check | one variant |
| `raster-runtime/src/backing.rs`, `storage.rs` | thread the expected schema to selection sites | moderate |
| `raster-runtime/src/source/file.rs` | ranged reads with a retained handle, replacing whole-file `fs::read` (`:268`) | moderate |
| `raster/src/input.rs` | **unchanged** — sweeping is `select!(List<BytesPage>, …)` plus the existing list recur (§1.3); the cursor and lazy resolution are [`lazy-list-recur.md`](./lazy-list-recur.md) and land before this | none |
| `raster-macros/src/lib.rs` | `#[page_size = n]`, registered as `#[proc_macro_derive(Selectable, attributes(page_size))]` or the field attribute will not compile; generated `*_PAGE_SIZE` const; `select!` target checking for `Bytes`/`BytesPage`/`Block<BytesPage>`; byte→page unit conversion and the literal-range alignment diagnostic; permit selecting the region's `page_size` field | additive |
| `raster-macros/src/recur.rs` | unchanged — sweeps are list recur | none |
| `raster-compiler/*` | unchanged — no new selector surface to carry | none |
| `raster-prover/guests/transition/src/checks/store.rs` | audit rules §3.1, where the witnesses are in scope (§3.2) | additive |
| `raster-cli` | fixture surface takes a page size | additive |
| **all guest images** | shared parsing changed ⇒ every image id, therefore every `program_commitment`, changes on this release | **migration** |

The last row is the one to socialize before merging: a breaking identity change for every
existing program, whether or not it uses `Bytes`.

## 5. Programming model and authoring hints

The vocabulary gains one rung:

| | storage-resident, iterated | materialized into a tile |
| --- | --- | --- |
| typed elements | `List<T>` | `Block<T>` (bound from the CFS) |
| raw bytes | `Bytes` | `BytesPage` (bound from the type's `#[page_size]`) |

1. **Choose `page_size` as a multiple of your record stride.** First constraint, ahead of
   every performance consideration. A record straddling a page boundary forces
   loop-carried stitching state in every tile and makes the last page a special case.
2. **Then size it against the replay budget.** Roughly
   `cycles ≈ page_size × (per-byte work + ~1.1 for the input-commitment hash)`; take the
   largest page that stays under budget, because per-replay fixed overhead amortizes.
3. **Never model byte data as `List<u8>`,** and never hex-encode into a `String`.
4. **Ranges for random access, recur for sweeps** (§1.4c).
5. **Sweep by selecting `.pages`, not the region.** `call_recur!` iterates a
   `List<BytesPage>`; `select!(Bytes, …)` is for holding a reference to the whole region
   (§1.3). One spelling, valid everywhere including across sequence boundaries.
6. **Keep layer/record offsets page-aligned at import.** It turns a runtime abort into an
   import-time invariant.
7. **Give a large region its own external input** with `load_preference: "mmap"`.
8. **Assert page invariants inside the tile** — `len % element_size == 0`. `len` is input,
   not a constant.
9. **Compute addresses from committed headers, never on the host** — a table inside the
   artifact, cited via `BoundIndex`, keeps the address in the authorization chain.

## 6. What this deletes

From the withdrawn `BoundedBlob` draft: the `BoundedBlob<MAX_BYTES>` type and its `TryFrom`
ceilings; `SchemaNode::Blob { max_bytes }`; MAX on the wire, in the index, and in the
commitment, with its three-way agreement check; `ExternalEncoding::RawBlob`; the
`Materializable` size-descriptor trait; CFS `max_input_bytes`/`max_output_bytes` (which
never existed); `SelectionProofStep::ByteRange` with `head`/`tail` witnesses.

From revision 1 of this proposal: `SelectionProofStep::Bytes`; `RasterNodeKind::Bytes`; the
`bytes-root` wrapper hash; the `Bytes` recur driver; the `pages = m` alias.

From revision 2: `IndexTransform`, the `transform` field on `BoundIndex`, and the `PageOf`
audit arm — byte→page conversion is a tile (§1.5).

Three decisions do all of that work: **the consumption unit is the commitment unit** (so
nothing is ever misaligned), **`Bytes` is a struct** (so nothing new is needed to prove,
index, or iterate it), and **address arithmetic is computation, so it lives in a tile** (so
nothing new is needed to audit it).

The trade-offs recorded in revision 1 have moved:

- **(a) resource envelope** — *restored, conditional on `InterfaceDecl.schema_hash`*
  (§2.1). Page size is declared in the type, so once the interface schema is committed a
  verifier reading `program.bin` can again see the replay unit size. Without that field the
  declaration is enforced at load but reaches no commitment, and this trade-off stays where
  revision 1 left it.
- **(b) absurd page size** — *mitigated*. An artifact whose page size disagrees with the
  declared one is rejected at load, before mapping.
- **(c) all programs inherit a granularity** — *unchanged and intended*: granularity is
  bound to the type definition.

Nothing here is a one-way door. The page list says nothing about how a consumer slices it,
so sub-page addressing could be added later as a new proof step against existing artifacts,
with no re-import.

## 7. Host execution and memory

- `load_preference: "mmap"` for any large region. Mapping does not allocate; a page
  selection is one slice plus one copy of `page_size` bytes.
- **`Read` mode must become ranged reads** (`source/file.rs:268` reads whole files today),
  or read mode cannot claim bounded host memory for a multi-page artifact.
- **Recur over pages must be index-driven** — the blocking dependency, specified separately
  in [`lazy-list-recur.md`](./lazy-list-recur.md). Recur tiles resolve the whole source into
  a `Vec<T>` (`raster/src/input.rs:1990-2005`) and recur sequences resolve the parent behind
  an `Rc` (`:1281-1300`); for a multi-GB page list that is not a performance bug but a
  correctness ceiling. It is a `List<T>` problem that every existing program already pays
  for and that `Bytes` merely makes fatal, which is why it ships first and on its own
  benchmark. It is a prerequisite of sweeping a region at all, independent of how the page
  list is spelled.
- Peak host RSS for a full pass moves from `O(artifact)` to `O(page + witness)`.

## 8. Performance estimation

Structural ratios below are exact arithmetic from the format. **Cycle figures are
order-of-magnitude and must be measured before being quoted.**

Worked example: 1 GiB region, i32 values, row stride 16 KiB, `#[page_size = 262_144]`
(16 rows/page) ⇒ `k = 4096` pages.

| | `List<u8>` | hex `String` chunks | **`Bytes`** |
| --- | --- | --- | --- |
| data file | ~10 GiB | 2 GiB | 1 GiB + 135 KB |
| index nodes | 1.07 × 10⁹ | 4 096 | 4 100 |
| index Merkle hashes | ~2.1 × 10⁹ (68 GB) | ~8 192 (256 KB) | ~8 192 (256 KB) |
| `.rindex` total | ~120 GB — **infeasible** | ~460 KB | ~460 KB |
| proof / page selection | ~30 × 32 B + payload | 12 × 32 B + 512 KiB | 12 × 32 B + 256 KiB |

Per replay unit (one 256 KiB page):

| axis | hex chunks | **`Bytes`** | change |
| --- | --- | --- | --- |
| tile input bytes | 512 KiB | 256 KiB | −50% |
| input-commitment SHA-256 | ~556 K cycles | ~278 K cycles | −278 K |
| hex decode | ~2.6–5.2 M cycles | 0 | −2.6–5.2 M |
| useful work (65 536 i32) | ~0.5–1 M cycles | ~0.5–1 M cycles | — |
| **total** | **~3.7–6.8 M** | **~0.8–1.3 M** | **≈ 3–5× fewer cycles** |

SHA-256 at ~1.06 cycles/byte with the risc0 accelerator; hex decode at 5–10 cycles per
output byte. The hex-decode term carries the headline — verify it first; a cheaper decoder
shrinks the ratio toward 2×.

Whole program, 4 096 pages:

| | hex chunks | **`Bytes`** |
| --- | --- | --- |
| total replay cycles | ~15–28 × 10⁹ | ~3.3–5.3 × 10⁹ |
| host peak RSS (full pass) | ~4–6 GB | ~1–2 MB above the mapping |
| bytes read from disk | 2 GiB | 1 GiB |

The host-memory row is the largest change and the least sensitive to estimation error: it
follows from `resolve_recur_list_source` materializing the entire parent list today versus
resolving page locations through the index. A change of asymptotic class, not a constant.

What does *not* improve: proving a given number of useful bytes still costs what it costs,
and retuning `page_size` still requires re-import plus an identity change.

## 9. Implementation order

0. **[`lazy-list-recur.md`](./lazy-list-recur.md), all phases** — ships first, on its own
   peak-RSS benchmark. Nothing below is measurable without the cursor work, and its completeness
   auditing gates the *claim* that a sweep covered a whole region — not this proposal's format
   or addressing work, which a `select!` at a computed page index authenticates on its own. Its
   phases are not independently releasable: stopping short of driver-level chunking leaves the
   repository's existing `chunk = N` recur broken.
1. `Bytes` (struct-shaped) and `BytesPage` types; `#[page_size = n]` attribute and the
   generated const; serde bridges in both serializers.
2. Payload tag `0x0B` and its `parse_subtree_root` arm; `rindex03`.
3. Encoder: `Bytes::paged` + `write_raster_files` emitting a paged region.
4. Load-time page-size conformance: schema threading into raster sources
   (`entry_arguments.rs:118-124`); page-shape check before the payload copy;
   `InterfaceDecl.schema_hash` (§2.1).
5. Sweep via the existing list recur over `.pages`; audit rules §3.1 in `checks/store.rs`.
6. Byte-offset addressing: `select!` support for the region's `page_size` field, and the
   `page_of` pattern documented in the skill. No audit change (§3.2).
7. Byte→page unit conversion and the literal-range alignment diagnostic in `select!`.
8. Selection ↔ replay root binding (§3.3) — release gate, generic, splittable.
9. Ranged reads for `Read` mode.

Phases 1–3 are self-contained and testable without touching the runtime selection path.
Phase 5 is the only one that touches audit logic, and it touches `checks/store.rs`, not the
proof verifier and not `verify_bound_index_bindings`.

## 10. Verification

- construction at `byte_len` = 0, `< page_size`, `= page_size`, `= page_size + 1`,
  `= k × page_size` exactly; `page_size = 0` rejected;
- artifact page size disagreeing with the declared `#[page_size]` rejected at load;
- direct encoding and draft set/finalize produce identical structural roots;
- forged artifacts: page `offset ≠ index × page_size`; short non-final page; page count
  disagreeing with `⌈byte_len/page_size⌉`; overlapping pages; `byte_len`/`page_size` leaf
  values inconsistent with the page list;
- `.rindex` flat as `byte_len` grows at fixed page size, and halving with doubled page size;
- byte-offset addressing: `page_of` at offset 0, at both page edges, and on the short final
  page; page index cited from a binding not on the step; cyclic citation; index exceeding
  `U64` width; a page index computed from a literal page size instead of the committed field
  surfaces as `external`/`Inline` in `cfs.json`;
- changing `#[page_size]` changes `program_commitment` (requires `InterfaceDecl.schema_hash`);
- range selection returning covering pages, including the `+1` unaligned case; unaligned
  literal range produces a compile diagnostic; emitted selector segments are in page units;
- sweep: iteration count, short final page, `chunk = m` shape rules;
- sweeping across a sequence boundary: a region passed into a sub-sequence and swept there
  produces the same trace as sweeping it in place (the regression test for why §1.3 rejects
  the trait adapter);
- `select!(Bytes, …)` used directly as a `call_recur!` input is a compile error, with a
  diagnostic naming `.pages`;
- lazy recur instrumentation proving only the current page is materialized (a counter, not
  RSS);
- native and guest output parity on a multi-page pass;
- old `rindex02` artifacts still readable, or a clean version error;
- UI tests: `Vec<u8>` tile argument rejected; `Bytes` tile argument rejected; a struct with
  a `Bytes` field rejected as a tile argument; `select!(BytesPage, region)` without an index
  rejected.

## 11. SKILL.md updates

**§2, after "Collections are `List<T>`; the only tile-visible window is `Block<T>`":**

> ### Raw bytes are `Bytes`; the only tile-visible window is `BytesPage`
>
> Byte data gets the same split, with the granularity declared on the field:
>
> ```rust
> #[page_size = 262_144]
> pub weights: Bytes,
> ```
>
> - **`Bytes`** — a paged byte region. `Selectable`, never `Materializable`.
> - **`BytesPage`** — one page, the only byte value that may cross a tile boundary. It
>   carries its committed `index()` and `offset()`.
> - To reach a byte offset, convert it to a page index **in a tile**
>   (`call!(page_of, offset, page_size)`), then index the region with the result. Selector
>   indices are page indices; arithmetic in a selector is still arithmetic. Pass the offset
>   to the consuming tile as well — `local = offset - page.offset()`.
> - Choose `page_size` as a multiple of your record stride so no record straddles a page,
>   then size it to the per-replay cycle budget.
> - Never model byte data as `List<u8>` (index larger than the data) or hex in a `String`
>   (2× everywhere plus decode cycles).

**§5 (`select!`):**

```rust
let weights   = select!(Bytes, model.clone().weights);
let page_size = select!(u64, model.clone().weights.page_size);
let page_idx  = call!(page_of, offset, page_size);   // byte→page is a tile, not a selector
let page      = select!(BytesPage, model.clone().weights[page_idx]);
let window    = select!(Block<BytesPage>, model.weights[524_288..1_048_576]);
```

> A byte range returns the whole pages **covering** it, so an unaligned range yields one
> page more than its length implies. Literal ranges are given in bytes and converted to page
> units at expansion, checked against the declared `#[page_size]` — so `cfs.json` shows page
> indices where the source shows byte offsets. Computed range bounds are not supported.

**§7 decision tree:**

| Need | Use |
| --- | --- |
| sweep a byte region | `call_recur!` with `input = <Bytes>` — it is a page list |
| several pages per replay unit | `chunk = N` (step takes `RecurInput<Block<BytesPage>>`) |
| the page at a computed byte offset | `call!(page_of, offset, page_size)` then `select!(BytesPage, region[page_idx])` |
| whole `Bytes` into one tile | **NEVER** (compile error — not `Materializable`) |
| recur driven by byte ranges | **NEVER** — page count wobbles and fails the chunk rules |

**§9 symptom table:**

| Symptom | Likely violated rule |
| --- | --- |
| `.rindex` far larger than the data file | byte data modelled as `List<u8>` instead of `Bytes` (§2) |
| "page size does not match declared" at load | artifact written with a different `#[page_size]` (§2) |

**`references/data-and-io.md`** — paged fixtures: `Bytes::paged`, the generated
`*_PAGE_SIZE` const, choosing page size (stride first, budget second), `mmap` preference,
and the fact that changing `#[page_size]` is both an artifact regeneration and an identity
change.

**`references/recur.md`** — sweeps over `Bytes`, why byte-range-driven recur is forbidden,
and straddling records as an argument for stride-aligned page sizes.

## 12. Implementation review (2026-08-14)

Review of the landed implementation against this document. The format, the struct shape
(§2.1), and the sweep path (§1.3) landed faithfully. What follows are defects, ranked.

The headline: **the guest-side geometry audit that §3.1 exists for is fail-open in the two
cases that matter** — range/chunked selections and single-page regions — and the serde
bridge §4 called for was never added, so every page still round-trips one `TreeValue` per
byte on the host.

### 12.1 Guest audit holes — `raster-core/src/input.rs:1068-1160`

**(a) Range and `chunk = N` selections skip the check entirely.**
`verify_bytes_page_geometry` opens with

```rust
let Some(...) = parse_bytes_page_payload(&witness.bytes) else { return true };  // :1069-1072
```

A range selection's payload is a `0x02` list header wrapping the concatenated pages
(`raster-runtime/src/input.rs:1370-1385`), never `0x0B`. It parses as `None` and is
**accepted unconditionally**. `chunk = N` recur drives `Block<BytesPage>` from range
selections — the spelling §1.3 and SKILL §7 recommend for "several pages per replay unit",
and the one `crates/raster/tests/paged_bytes.rs:95` exercises. That sweep gets no
page-shape auditing at all. Same for `select!(Block<BytesPage>, weights[a..b])`.

This is the largest gap: rule 3 is what turns rule 2 from a count into a partition, and the
recommended sweep spelling bypasses it.

**(b) A single-page region never validates `page_size`.**

```rust
} else {                                   // index == 0 && last
    if page_offset != 0 { return false; }
    let byte_len = len;
    if u64_leaf_root(byte_len) != siblings[0] { return false; }
    return page_count == 1;                // :1137 — always true in this branch
};
```

`siblings[1]`, the committed `page_size` leaf, is never touched. An artifact committing
`page_size = 1, byte_len = 5` while shipping one 5-byte page passes the guest audit; the
correct partition is five pages. The host catches it
(`backing.rs::check_schema_page_sizes` → `check_page_partition`), but the host is not the
trust boundary.

**(c) Fail-open by default, and value-matched proof steps.**

- `:1103` — if no `Struct` step has field names exactly `["byte_len","page_size","pages"]`,
  the function returns `true`. Unrecognized ⇒ accepted.
- Rule 2 (`page_count == byte_len.div_ceil(page_size)`) runs only on the **last** page
  (`:1155`). A sweep that stops before the final page never checks count against `byte_len`.
- `:1090` matches *any* `List` step whose `index` equals the page index — for
  `models[1].weights.pages[1]` the outer list step is a candidate, and last-match-wins
  happens to save it. `bytes_step` is likewise whichever qualifying struct step comes last.
  Both should be positional (the step immediately enclosing the payload), not value-matched.

§3.2 argues this principle in as many words — *"new surface the audit cannot name is unsound
by default"*. The implementation does the opposite three times.

### 12.2 Pages serialized as one value node per byte — **resolved**

A page reached the selection tree by being serialized with the *general-purpose*
`TreeValueSerializer` and pattern-matched afterwards:

```rust
let inner = value.serialize(TreeValueSerializer)?;   // full generic tree
if name == BYTES_PAGE_NEWTYPE_NAME {
    return tree_bytes_page_from_wire(inner);         // destructure it straight back
}
```

`BytesPageWire { …, bytes: Vec<u8> }` uses a plain derive, so serde emits `serialize_seq` and
each byte became a `TreeValue::U8` / `DraftValue::U8`. The decode direction did the mirror
image: `bytes_page_wire_fields` expanded a flat payload into `List([U8; n])` purely so
`OwnedStructAccess` could collapse it back into the `Vec<u8>` that `OwnedU8SeqDeserializer`
already wanted.

`size_of::<TreeValue>() == size_of::<DraftValue>() == 56`, so the transient intermediate was
**56× the payload** — measured at 61–62× with allocator slack, 64 MB for a 1 MiB page —
built, walked once, and discarded, per page, per direction. This was §Problem item 1's
`List<u8>` cost reintroduced in the encode/decode path, and it is why §8's host-RSS row was
unreachable.

**The format was never affected.** A page is hashed once (`bytes_page_root`), carries one
`Leaf` index node, and the `0x0B` payload is flat. The blowup was entirely in the
Rust-value ↔ tree-value bridge, before anything hashed. Guest cycle figures were untouched.

**Root cause: a pattern that is correct for `List<T>` and wrong for a page.** The
serialize-then-match shape is inherited from the `ListHandle` arm one line up, where it is
free — `ListHandle(values)` re-wraps the very `Vec` that had to be built anyway. For a page
the intermediate is 56× and entirely thrown away. The `name` is a parameter of
`serialize_newtype_struct`, available *before* the inner value is serialized; the code simply
did not act on it until after.

**Fix (landed).** Recognize the page from its newtype name first, and hand the inner value to
a narrow serializer that knows the page shape statically —
`raster_core::collections::bytes_page_parts`, one implementation shared by both bridges. Its
`SerializeSeq` collects the payload into a flat `Vec<u8>` instead of a node per byte;
everything that is not the page wire struct is an error. `BytesPage::serialize` now emits a
borrowing `BytesPageWireRef`, which also drops a full-page clone per serialize. On the decode
side `OwnedStructAccess` carries its own `PageWireField` instead of `TreeValue`, so the
payload passes through as one buffer. `tree_bytes_page_from_wire`, `draft_bytes_page_from_wire`,
`tree_bytes_field`, `draft_bytes_field`, and the two `*_u64_field` helpers are deleted.

**What deliberately did *not* change:** neither `serialize_bytes` arm. Both still refuse raw
bytes, so §Problem's invariant stays *structural* — bytes are unrepresentable in the general
serializers, not merely rejected later. This is why the fix does not follow §4's
"`serialize_bytes` arm" line item: that would have opened a general bytes channel and moved
the invariant from impossible to checked. Also unchanged: the structural root, the `0x0B`
payload, `rindex03`, and the postcard wire — verified byte-identical, since postcard encodes
`u8` as one raw byte and a seq as varint-length plus elements. `input_commitment` and image
ids do not move; this is an optimization, not a migration.

**Tests.** `collections.rs`: postcard wire pinned to spec-derived bytes, round-trip across
every page shape, `bytes_page_parts` rejects non-page values, and raw bytes are still refused
by the general serializers. `raster-runtime/src/input.rs`: draft and direct encoding agree on
a page's structural root (a §10 item that had no test). `raster-runtime/tests/bytes_page_alloc.rs`:
a `#[global_allocator]` guard asserting a 1 MiB page encodes and drafts under 12× peak —
confirmed to fail at 61–62× against the pre-fix code, which is the only reason it is worth
having.

### 12.3 Divergences from the specified surface

| | Issue |
| --- | --- |
| (a) | **`#[page_size = n]` is decorative.** Page size lives in `Bytes<N>`; the attribute is optional and only cross-checked for equality (`raster-macros/src/lib.rs`, `page_size_consts`). All three jobs §1.1 assigns it are done by the const generic. It is now a hand-maintained duplicate of the type parameter. |
| (b) | **Unaligned literal ranges are not a compile diagnostic** (§1.4c, phase 7). It is a post-monomorphization `const {}` panic inside `page_range_for_*`, with no span on the `select!`. `crates/raster/tests/ui/select_unaligned_byte_range.rs` **does not test `select!`** — it hand-writes `const _: () = assert!(...)` against `<Bytes<4> as PageSized>::PAGE_SIZE` and lets the actual `select!(Block<BytesPage>, model.weights[1..5])` compile clean. The test is a placebo. |
| (c) | **Byte-vs-page units depend on spelling.** `weights[262144]` is a byte offset; `weights.pages[262144]` is a page index. Same target type, silently different meaning, decided by `already_pages` in `emit_selector_segments`. Nothing rejects or warns on the second form. |
| (d) | **No page-shape check at the selection site.** §4 promised one in `selected_payload_from_raster_location`; there is none. Host-side only the partition *count* is checked, in `backing.rs::check_schema_page_sizes`, and only for the first element of any enclosing list. |
| (e) | **`base_expr` is emitted twice** — once inside `page_index_for_*(&#base_expr)` and once in `select_source(#base_expr, …)`. For `model.clone().weights[0]` that is two clones and two evaluations of any side effect. |

### 12.4 `schema_walk` is a second implementation of `Selectable::schema()`

`crates/raster-compiler/src/schema_walk.rs` re-derives schemas from source text. It diverges
from the derive in ways nothing cross-checks:

- **ignores `#[schema(tag = N)]`** — the derive sets `SchemaField.label` from the tag
  (`raster-macros/src/lib.rs:3596`), the walk always uses the field name. Any struct using
  the attribute gets a wrong `schema_hash` committed, silently.
- no enums, no tuple structs, no generics; `leaf_schema` omits `f32`/`f64`/`u128`/`char`
- resolves struct names by bare ident across all of `src/`, so two same-named structs in
  different modules collide

**And it is a compile regression.** `fill_schema_hashes` returns
`Err("unknown interface type X: no matching struct in src/")` for anything it cannot
resolve, and it sits on the `assemble_program` path for *every* project
(`raster-cli/src/program.rs:188`). Any existing program whose `main` takes an enum, a tuple
struct, an `f64`, or a type from a dependency crate now fails to build. `hello-tiles`
survives only because it happens to use `List`/`String`/`usize`.

Separately, `InterfaceDecl.schema_hash` is `#[serde(default)]`, so an old `program.bin`
deserializes with a zero hash rather than producing a version error — the opposite of the
`rindex02` hard-break decided in open question 4.

### 12.5 Runtime

- **`tracing/recorder.rs:1103-1109` passes a stub schema** (`Leaf { type_name: "" }`) for
  every Raster source, so §3.1 rule 1 is a no-op in the trace/replay path. The adjacent
  Postcard arm carries a 15-line comment explaining its constraint; this one has none.
- **`SourceFile::bytes()` panics on IO error** inside `get_or_init`
  (`source/resolved.rs`) instead of returning `Error` — `OnceLock` cannot hold a `Result`,
  so the fallible path was papered over.
- **`read_range`'s non-unix branch** binds the mutex guard, drops it, then re-locks. Dead
  code on Linux, but wrong as written.
- **mmap selections now copy.** `RasterData::read_subtree` returns `Vec<u8>`, so leaf reads
  that previously borrowed from the mapping allocate. A whole-region or root selection still
  walks every page node into RAM, so §7's `O(page + witness)` claim holds only for
  page-granular selects.

### 12.6 §10 coverage gaps

No test exists for: byte-offset addressing end-to-end (no `page_of`, no
`select!(u64, region.page_size)`, no `select!(BytesPage, region[page_idx])` — phase 6 is
entirely uncovered); range selection returning covering pages, or the `+1` unaligned case;
`#[page_size]` changing `program_commitment`; draft set/finalize producing the same
structural root as direct encoding for a `BytesPage`; forged single-page or range witnesses
against the guest check (the four hand-built witnesses at `raster-core/src/input.rs:1880+`
cover only the multi-page non-final and final cases).

### 12.7 Fix order

1. §12.1(a) and (b) — the audit is the trust boundary; a chunked sweep proving nothing
   about page shape is the release blocker.
2. §12.4 — the `assemble_program` regression breaks existing projects on this release,
   independent of `Bytes`.
3. §12.1(c), §12.3(b), §12.6 — fail-open arms, the placebo UI test, and the missing
   phase-6 tests.
4. §12.3(a)/(c)/(d)/(e) and §12.5 — surface and runtime cleanups.

~~§12.2~~ — **done.** The per-byte value tree is gone in both bridges; §8's host-memory row
is now reachable on the encode/decode path.

Note that §3.3 (selection↔replay bind) and the lazy-list-recur blocking dependency are both
still open, so `Bytes` is not end-to-end authorization-sound yet regardless of the above.

## Open questions

1. **Should `page_of` be a framework-provided tile** rather than one every program writes?
   A `raster::tiles::page_of` in the prelude would be one image id shared across programs
   and one less place to get the division wrong — but it puts a framework tile in the user's
   image registry, which nothing does today.
2. **Should `#[page_size]` accept a non-literal** (a const path, e.g.
   `#[page_size = ROW_STRIDE * 16]`)? Useful for keeping the stride relationship visible;
   the macro needs a const-eval story.
3. **`BoundRange` for computed range bounds** — deferred to v2. Worth doing only when a
   program needs a variable-length window at a computed offset.
4. ~~**`rindex02` compatibility window**~~ **Resolved: hard-break.** `rindex02` is a clean
   version error; re-import as `rindex03`.
5. ~~**Is `InterfaceDecl.schema_hash` in scope**~~ **Resolved: in scope.** The compiler
   fills it from an AST schema walk; it is not authored in `Raster.toml`.
6. ~~**A `pages!(weights)` sugar macro**~~ **Resolved: no.** Sweeps are
   `select!(List<BytesPage>, region.pages)` then `call_recur!`. A trait adapter that
   rewrites the selector is invisible to the flow resolver (§1.3).
7. ~~Payload tag allocation.~~ **Resolved: `0x0B` here, `0x0A` for list metadata in
   `lazy-list-recur`**, with the canonical table recorded in `parse_subtree_root`'s doc comment
   rather than split across proposals. Revision 2's struct shape freed `0x0A` by dropping the
   region handle.
