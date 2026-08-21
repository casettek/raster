# Proposal: `bounded-collections` — `List<T>`/`Block<T>`, collections that cannot cross a tile boundary

Status: Phase 1 + Phase 2 implemented (2026-07-29; proposed 2026-07-28)
Related: [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) — the skill
currently enforces these rules as prose; this proposal turns the load-bearing ones into
types. Complements [`program-identity.md`](./program-identity.md): identity pins *which
code* runs; this pins *how much data* any one replay unit can touch.

## Problem

The model's core guarantee — every replay unit touches a bounded amount of data — is
documentation, not mechanism. Four concrete holes:

1. **Tile signatures accept any serde type, including `Vec<T>`.** A tile declared as
   `fn score(rows: Vec<Row>)` compiles, runs natively, and materializes the entire
   collection into one replay unit. Every argument funnels through
   `impl IntoAuthValue<T>` (`rewrite_into_auth_value_args`,
   `crates/raster-macros/src/lib.rs:300`) and `into_auth_value::<T, _>`
   (`gen_auth_value_materialization`, `lib.rs:329`) — and nothing at that funnel
   distinguishes a scalar from an unbounded sequence. The prohibition lives only in the
   skill's prose ("whole large collection into one tile — **NEVER**"). Return types
   have the same hole in the other direction: a tile returning `Vec<Row>` pays whole-
   collection Merkleization on every call, unchecked.

2. **The inline-literal door.** The blanket
   `impl<T: Serialize> IntoAuthValue<T> for T` (`crates/raster/src/input.rs:1281`)
   accepts a computed `vec![a, b]` from a sequence body as an unauthenticated inline
   argument. Forbidden by the sequence grammar in prose; type-checks in practice.

3. **The recur `args` smuggling channel.** Passing a collection through recur-tile
   `args` materializes it into *every* iteration's replay unit — the
   `references/recur.md` "forbidden pattern". Recur-tile extra args go down the same
   `IntoAuthValue` path as any tile argument, so nothing stops it. (Recur-*sequence*
   extra args, by contrast, already travel as references — `IntoAuthRef`,
   `crates/raster-macros/src/recur.rs:839` — which is exactly the asymmetry the model
   wants, unenforced.)

4. **One type conflates two orthogonal properties.** The schema layer already treats
   lists as first-class nodes: `SchemaNode::List` with element schemas,
   `SchemaFieldMode::AppendOnlyVec`, and dedicated list Merkle proof steps
   (`ListProofSibling`, `crates/raster-core/src/input.rs:58-90,162-171`), and
   `Vec<T>: Selectable` (`input.rs:886`) is how `select!` paths traverse them. But the
   same `Vec<T>` is also an ordinary serde type that deserializes whole by
   construction. In one type, *selectability* (reach into it while it stays in
   storage) and *materializability* (bring it out whole) are fused — so anything
   selectable is also passable-whole, which is precisely the hole. The commitment
   layer knows a list is a tree of separately addressable elements; the Rust type
   insists on materializing all of them to exist at all.

Every violation above compiles and runs natively; it fails at prove time (cycle blowup)
or never fails at all — it just silently produces programs whose per-step cost is
unbounded and whose fraud proofs are unaffordable.

## Goal

Make the unbounded-materialization states **unrepresentable** instead of forbidden:
remove `Vec` from the Rastered-type vocabulary entirely and split its two conflated
properties into two traits and two collection types.

The two properties, named as duals:

- **`Selectable`** (exists today) — the *reference* side: can this type be addressed
  **into** while it stays in committed storage? (schema, paths, proofs)
- **`Materializable`** (new) — the *value* side: may this type leave storage **whole**
  and enter a replay unit? (bounded decode + commit)

The two collection types, one per side of the split:

- **`List<T>`** — the unbounded collection. Selectable, **not** Materializable.
  Exists to be *referenced*: selection source, recur input, draft target. Never
  materializes whole; not materializable into a tile — not by exemption, but because
  it does not implement the materialization path.
- **`Block<T>`** — a bounded, materialized window into a list. Selectable **and**
  Materializable — the **only** collection type that crosses a tile boundary. Only the
  framework can construct one, and only from operations whose size bound is pinned as
  a literal in the CFS.

The resulting type lattice:

| Type | Selectable | Materializable | i.e. |
| --- | --- | --- | --- |
| scalars, Rastered structs | yes | yes | reach in, or pass whole |
| `Block<T>` | yes | yes | a bounded window — both sides |
| `List<T>` | yes | **no** | reach in, never pass whole |
| external serde types | no | no | not part of the model at all |

`Vec<Row>` being *neither* Rastered word is the point.

Design decisions taken:

- **Vocabulary over enforcement.** Rejecting `Vec` at the type-definition level (the
  `Selectable` derive) is earlier and simpler than policing the tile boundary with
  escape hatches. There is nothing to exempt when the word does not exist.
- **Boundedness is a type, and its proof is the constructor.** `Block<T>` has no public
  constructor reachable from authorized sequence flow; its two birthplaces — literal
  range `select!` and `chunk = N` recur drivers — are exactly the two doors whose bound
  the CFS pins. Tile-side construction is permitted and runtime-scoped (§ edge cases).
- **`Materializable` is the wall, on both directions of the boundary**: asserted for
  every plain tile argument AND the tile return type at compile time — violations fail
  at `cargo check`, rung 1 of the check ladder. Implemented for scalars, `String`,
  derived structs, and `Block<T>`; never for `List<T>` (or `Vec<T>`, which no longer
  appears).
- **Align the surface with the schema layer, then improve the layout underneath.**
  Phase 1 introduces the types over the existing flat serialization (pure vocabulary +
  enforcement, small diff). Phase 2 gives `List<T>` the chunked commitment layout the
  schema layer already implies — invisible to user code, only the proofs get better.

## The vocabulary

```rust
// input.rs — the Rastered data model
#[derive(Serialize, Deserialize, Selectable)]
pub struct PersonalData {
    pub name: String,
    pub addresses: List<Address>,   // not Vec — maps to SchemaNode::List
}
```

`List<T>` (raster-core, no_std):

- Serde-compatible for **host-side ingestion only** (a `List<T>` field deserializes
  from a JSON array in `gen_input` fixtures; external data still arrives as arrays).
- In committed storage it *is* the `SchemaNode::List` node it already maps to; it
  takes over the `Selectable` impl currently on `Vec<T>` (`input.rs:886`), which is
  retired in phase 1 so the old spelling fails at the `select!` site, not as a
  dead-end binding downstream.
- Implements `Selectable`, never `Materializable`. Its legal consumers: `select!`
  paths (element, range), `call_recur!`/`call_recur_seq!` input, draft append targets,
  and reference-typed recur-sequence args.

`Block<T>` (raster-core, no_std):

```rust
/// A bounded run of elements. Constructed by the framework from operations whose
/// size bound is pinned as a literal in the CFS, or by tile code at runtime
/// (guarded — see below).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block<T> {
    items: Vec<T>,               // private — no public From<Vec<T>>
}

impl<T: Materializable> Materializable for Block<T> {}

impl<T> Block<T> {
    #[doc(hidden)]
    pub fn __from_selection(items: Vec<T>) -> Self { Self { items } }  // generated code only
    /// Tile-side constructor. Asserts an active tile execution scope at runtime:
    /// a sequence-level `Block::build` fails on the first native run (rung 3).
    pub fn build(items: Vec<T>) -> Self { /* assert_tile_scope(); */ Self { items } }
    pub fn iter(&self) -> core::slice::Iter<'_, T> { self.items.iter() }
    pub fn into_vec(self) -> Vec<T> { self.items }
}
impl<T> core::ops::Deref for Block<T> { type Target = [T]; /* … */ }
```

`Materializable` (raster-core, no_std):

```rust
/// A value small/bounded enough to be materialized whole into one replay unit.
/// Dual of `Selectable`: `Selectable` reaches INTO a value in committed storage;
/// `Materializable` permits the value to leave storage WHOLE.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be materialized into a tile: collections are iterated, not passed whole",
    label = "not materializable",
    note = "make the collection the `input` of `call_recur!` (one element per step, or `chunk = N` blocks)",
    note = "for a bounded slice, use `select!` with a literal range `xs[a..b]` — it yields a `Block<T>`"
)]
pub trait Materializable {}
```

Implemented for primitives, `String`, `Option<T: Materializable>`, `[T; N]`, small
tuples, `Block<T: Materializable>`, and derived user structs. The trait has no methods
— it is pure evidence, costs nothing at runtime, and trait resolution sees through
type aliases (`type Rows = Vec<Row>` cannot dodge it the way a syntactic AST check
could).

Why a new trait instead of reusing `Selectable`: `Vec<T>: Selectable` exists and must
exist for path traversal, and `List<T>` must likewise be Selectable while being
exactly the thing that must not materialize. The properties are orthogonal (see the
lattice above); requiring `Selectable` at the boundary would admit every list. The
derive is also the only gate that can vouch for *foreign* types: the materialization
funnel today requires only `Serialize`, so any external serde struct with a `Vec`
inside sails through — `Materializable`, granted solely by the Raster derive after
per-field checks, is what forces everything crossing the boundary into the vetted
vocabulary.

## Enforcement points

Five, from earliest to latest; the first two carry the load.

1. **The `Selectable` derive rejects `Vec` fields** (`derive_selectable`,
   `crates/raster-macros/src/lib.rs:2767`) with a domain-language error:

   ```
   error: `Vec<Address>` is not a Rastered type — collections in Raster data are
          `List<Address>`; tiles receive bounded `Block<Address>` windows of it
   ```

   The derive also emits `impl Materializable` for the struct **iff** every field is
   `Materializable` (a per-field `assert_field::<FieldTy>()` const block) — a struct
   containing a `List` is `Selectable` but not `Materializable`, so it cannot cross
   into a tile whole (§ edge cases below).

2. **The `#[tile]` macro asserts `Materializable` per plain argument AND on the
   return type.** The macro already classifies parameters (`param_protocol_kind`,
   `lib.rs:276`): `Draft`/`RecurOutput` take the draft path; `RecurInput`/`RecurState`
   are framework-threaded and shape-validated. For the remaining (plain `AuthValue`)
   parameters, and for the return type (unwrapping `Result<T>` and the recur protocol
   return forms), it emits:

   ```rust
   const _: () = {
       fn __raster_assert_materializable<T: ::raster::Materializable>() {}
       fn __raster_tile_boundary_check_score() {
           __raster_assert_materializable::<Row>();   // spanned to the user's type token
           __raster_assert_materializable::<u64>();   // return type included
       }
   };
   ```

   `quote_spanned!` on the type token puts the error on the offending signature. This
   covers recur-tile extra args (hole 3) — they are plain params — and closes the
   return direction of hole 1: a tile cannot return `Vec<Row>`; a tile that produces a
   collection returns a `Block<T>` or builds a `List` field through a draft.

3. **The call boundary gets a bounded trait.** One changed line in
   `rewrite_into_auth_value_args` (`lib.rs:307`):

   ```rust
   pat_type.ty = syn::parse_quote!(impl ::raster::IntoMaterialized<#ty>);
   ```

   with

   ```rust
   pub trait IntoMaterialized<T: Materializable>: IntoAuthValue<T> {}
   impl<T: Materializable, A: IntoAuthValue<T>> IntoMaterialized<T> for A {}
   ```

   `IntoAuthValue` stays as the untyped mechanism (recur internals, state threading);
   `IntoMaterialized` is the only spelling a tile boundary uses. This narrows hole 2:
   `call!(f, vec![a, b])` stops type-checking. It does **not** close the general
   computed-argument problem — an inline `Materializable` value
   (`call!(f, Config { .. })`) still passes and remains what it is today: a sequence-
   grammar violation caught by review and by its `external` binding in `cfs.json`.
   The claim here is exactly: *collections* can no longer enter inline.

4. **Recur input becomes typed.** The runtime check
   `"call_recur! requires a selectable storage list source"`
   (`crates/raster/src/input.rs:1077,1148`) becomes a compile-time requirement that
   `input` is a `List<T>` binding — an `AuthRef<List<T>>` from a `select!`, a draft-
   finalized `List` field, or a recur-sequence arg (the lineage is intact; see
   patterns below). The old "prior `call!` binding whose value is a `Vec<T>`" source
   disappears with `Vec` returns (point 2); its replacement is the draft-built `List`.

5. **`Block::build` is runtime-scoped.** Sequences are plain Rust that compiles, so a
   tile-visible constructor is also sequence-visible; without a guard, a sequence
   could compute a `Block` and pass it inline — a narrower reincarnation of hole 2.
   `Block::build` therefore asserts an active tile execution scope
   (`TileExecutionScopeGuard`, `crates/raster/src/lib.rs:133`): sequence-level
   construction fails on the first native run (rung 3), in addition to standing out as
   an `external` binding in the CFS.

## What owning the serialization buys (phase 2)

The schema layer already stores lists as trees with per-element addressing and list
proofs; `Vec`-as-postcard-sequence throws that away at every materialization. With
`List<T>` as the surface type, the layout underneath can match the model:

- **A struct's materialization carries its list fields as `(root, len)`, not
  elements.** Selecting or materializing `PersonalData.name` no longer decodes or
  Merkleizes `addresses` at all. Whole-struct selection stops being the cost trap the
  skill warns about; the warning becomes moot.
- **Selection proofs get their natural shape.** `select!(Address, addrs[1])` is an
  O(log n) inclusion proof against the list root; `select!(Block<Address>, addrs[0..4])`
  a contiguous slice proof — the artifacts the transition/fraud guests already verify,
  now native to the layout instead of derived from a flat byte sequence.
- **Drafts append against the same tree.** `output.lines().push(v)`
  (`SchemaFieldMode::AppendOnlyVec`) extends the chunked tree and pays the increment —
  the cost the draft protocol already promises, now structural.

Phase 2 changes no user-facing types or grammar — only commitment internals and proof
formats. Its blast radius therefore includes every verifier of those formats: the
transition guest's selection-proof checks and the `chain_fraud` guest's slice-proof
binding, plus recorded fixtures (`hello-tiles` etc.). It changes commitments, so it
lands before identities/checkpoints are durably committed anywhere — same reasoning as
`program-identity.md`.

## Grammar and patterns — what changes for authors

Nothing structural; the types now say what the prose said:

```rust
let addrs = select!(List<Address>, data.addresses);      // reference — cheap
let one   = select!(Address, addrs[1]);                  // element
let win   = select!(Block<Address>, addrs[0..4]);        // bounded window → tile-passable
let done  = call_recur!(tile = score, input = addrs, state = Score::default(), args = ());
```

- `chunk = N` recur steps take `RecurInput<Block<T>>` (was `RecurInput<Vec<T>>`).
- Tiles that *produce* collections: return a `Block<T>` (bounded, committed like any
  output) or build a `List` field through a draft / `RecurOutput`. Returning `Vec` is
  a compile error (enforcement point 2).
- The **two-collection pattern** (iterate A, scan B per element) is expressible and
  safe by construction: outer `#[sequence(kind = recur)]` over `List<A>`, with
  `List<B>` passed as a recur-sequence extra arg. Recur-seq args travel as `AuthRef`s
  (`recur.rs:839`) — passing the list costs nothing and materializes nothing; inside,
  `call_recur!(tile = scan_b, input = b_list, …, args = (item,))` iterates B with A's
  element as a scalar arg. Each replay unit touches one element of A × one
  element/chunk of B. The current skill-level red flag "input = something that arrived
  as an arg" narrows to *materialized* args; reference-typed recur-seq args become
  sanctioned dataflow.

Compile error an author sees for the classic violation:

```
error[E0277]: `Vec<Row>` cannot be materialized into a tile: collections are iterated, not passed whole
  --> src/lib.rs:12:20
   |
12 | pub fn score(rows: Vec<Row>) -> u64 {
   |                    ^^^^^^^^ not materializable
```

## Edge cases and their rules

- **Struct with a `List` field as a tile argument: forbidden in v1** (the derive makes
  it non-`Materializable`); authors select the scalar fields, which the cost rules
  demand anyway. Later refinement, enabled by phase 2: materialize the `List` field
  *as its handle* `(root, len)` so a tile can read `items.len()` (committed, sound)
  but never elements — the field stays Selectable without becoming Materializable, and
  the duality still parses. A property `Vec` cannot have.
- **Tiles constructing `Block`s: allowed, runtime-scoped.** The bounded-doors argument
  protects materialization *into* tiles; a tile's output Block is committed like any
  output, its cost visible and accounted. The tile-scope assert in `Block::build`
  (enforcement point 5) keeps the constructor out of sequence flow.
- **`Block` in recur `state`: legal but growing state remains an authoring error**
  (O(N²) committed bytes — `references/recur.md` §2). The type system bounds a single
  materialization, not growth across iterations; that rule stays doc-level.
- **`String` stays `Materializable`.** It is technically unbounded; pragmatically it
  is a scalar. Revisit only if it becomes an abuse channel.
- **Tile-local `Vec` use: unrestricted.** Inside a tile body plain Rust computation is
  free to use `Vec`; the vocabulary governs what crosses boundaries and what Rastered
  data *is*, not tile internals.

## Migration

Straight to the vocabulary — no intermediate "ban Vec but keep it" release. The derive,
formats, and all programs are in-house; migrating the examples once is cheaper than
shipping an enforcement regime twice.

1. **Phase 1 — types + enforcement, flat serialization.** `List`/`Block`/
   `Materializable` in raster-core (with `List` serializing exactly as `Vec` does
   today, so commitments do not move); the `Selectable` impl moves from `Vec<T>` to
   `List<T>` (`input.rs:886` retired); derive rejection + `Materializable` emission;
   `#[tile]` boundary assert (args + return); `IntoMaterialized`; typed recur input;
   `select!` range lowering yields `Block`; chunk driver hands `RecurInput<Block<T>>`;
   `Block::build` tile-scope guard. Port `raster-examples`, regenerate fixtures,
   update the skill (`references/data-and-io.md`, `references/recur.md`, §5/§7 of
   SKILL.md) and the check-ladder symptom table (several runtime symptoms become
   compile errors).
2. **Phase 2 — chunked commitment layout for `List`.** Struct nodes commit list fields
   as `(root, len)`; element/slice selection proofs go O(log n); draft appends extend
   the tree. Changes commitments and proof formats; user code unchanged; transition
   and `chain_fraud` guests updated in the same release (they verify the proof
   formats), fixtures regenerated.

If any external program exists at flip time, a `legacy-vec-args` feature can gate
`impl<T: Materializable> Materializable for Vec<T>` plus derive leniency for one
release; the default plan is not to need it.

## Phase 1 implementation notes (2026-07-29)

Landed across `raster-core`, `raster-macros`, `raster`, `examples/hello-tiles`, and the
`raster` integration tests. Verified through the check ladder: `cargo check` in both
postures, `cargo raster cfs`, native run, and the commit/audit round-trip
(`Verification Success`). All `raster`/`raster-core`/`raster-macros` tests pass, plus a
new `tests/ui/select_block_requires_range.rs` compile-fail snapshot.

What differs from the design above, and why:

- **The struct `Materializable` impl is emitted conditionally by syntactic detection,
  not as a where-bounded impl over all fields.** Rust eagerly evaluates concrete
  (non-generic) `where` bounds on an impl, so `impl Materializable for PersonalData
  where List<Address>: Materializable {}` hard-errors at the *definition* of a
  List-containing struct — which would break its `Selectable` use. Instead the derive
  checks for a direct `List<T>`/`Vec<T>` field: if present, it emits **no**
  `Materializable` impl (the struct stays `Selectable` only); otherwise it emits the
  where-bounded impl, whose concrete bounds hold for scalar/`Block`/materializable-struct
  fields. Consequence: a struct whose non-materializability is only *transitive* (a
  field that is itself a List-containing struct) currently hard-errors at definition
  rather than silently becoming non-materializable. Acceptable and safe (over-strict,
  never unsound); revisit with a lazy-bound encoding if a real program hits it.

- **`Block::build` ships without the runtime tile-scope assertion (enforcement point
  5).** The assertion needs tile-scope state that lives in `raster-runtime`, above the
  no_std `raster-core` where `Block` is defined; wiring a cross-crate hook was deferred
  as the point is explicitly defense-in-depth. A sequence-level `Block::build` is still
  caught as a computed argument (an `external` binding in the CFS), exactly as today.

- **`List<T>`/`Block<T>` are serde-transparent newtypes over `Vec<T>`** (Phase 1 keeps
  the flat wire layout, so bytes are unchanged), with `Debug` delegated to the inner
  slice so they print like the `Vec` they replace. Their `Selectable` schema is
  `SchemaNode::List` (moved off `Vec<T>`, whose `Selectable` impl is retired), with
  `type_name` `"List"`/`"Block"`. That string rename does move schema/commitment hashes,
  so fixtures were regenerated; the flat *value* layout is unchanged, and the chunked
  layout remains Phase 2.

Real behavior surfaced during the port: `personal_greet_from_object` used to take a
whole `PersonalData` (which now transitively contains a `List<Address>`); under the new
rules that struct is `Selectable` but not `Materializable`, so the tile was rewritten to
take the `name: String` field it actually reads — the v1 "select the field, not the
struct" rule in practice.

## Phase 2 implementation notes (2026-07-29)

Landed across `raster-core` (`input.rs`, `collections.rs`), `raster-runtime`
(`input.rs`, `storage.rs`), and the `hello-tiles` fixtures. Verified through the
check ladder: `cargo check` in both postures, `cargo raster cfs`, native run, the
commit/audit round-trip (`Verification Success`), and `program --verify`
(`Verified against Raster.lock ✓` — identity unchanged). All
`raster-core`/`raster-runtime`/`raster` tests pass, plus the transition guest's
native suite (37).

The chunked layout is realized as a **list-handle payload node** rather than a
whole storage-model change. A `List<T>` **field** of a struct encodes in
`data_bytes` as node `0x09 [root:32][len:8][inner_len:8][inline 0x02 list]`.
Core's `parse_subtree_root` reads the stored root and *skips* the inline region,
so a parent struct's structural root is O(1) in the list — selecting or
`payload_structural_root`-ing `PersonalData.name` no longer Merkleizes
`addresses`, the headline Phase 2 win.

Two decisions differ from the literal design above, both taken deliberately
(recorded in the proposal's own fork discussion):

- **Elements stay inline under the handle; they are not removed from the
  parent payload.** The design's "carries `(root, len)`, *not elements*" would
  shrink the materialized bytes but requires list elements to live in a
  separately-resolvable storage location (structs today carry their lists'
  bytes, and `select!`/chain/audit read elements straight out of the parent via
  `raster_subtree_bytes`). Keeping elements inline gives the O(1) root win
  without a coordinate-resolution overhaul; the materialized bytes do not
  shrink. A handle whose stored root disagrees with its inline elements is
  caught the moment anything selects into the list (the element proof folds to a
  different root), so trusting the stored root for the O(1) path is sound.

- **The distinction is carried by serde, not by threading the schema through the
  encoder.** `List<T>` serializes via `serialize_newtype_struct` under the
  sentinel name `LIST_HANDLE_NEWTYPE_NAME` (`$raster::ListHandle`). Postcard and
  serde_json ignore the name and stay byte-for-byte transparent (so wire, JSON,
  and the postcard structural commitment are unchanged — program identity did
  not move). Only the selection-tree serializer keys on it, producing
  `TreeValue::ListHandle`; a `Block<T>` (a plain seq) stays an inline
  `TreeValue::List`.

Layout detail that keeps the blast radius small: the handle wrapper exists
**only** inside the parent struct's bytes. The list's own index node points past
the 49-byte header at the inline `0x02` list (`enter_raster_frame` /
`prepare_raster_children` shift by `LIST_HANDLE_HEADER_LEN`), and
`finalize_raster_kind` maps `ListHandle → RasterNodeKind::List`. So every
downstream consumer — element/range selection, recur iteration, decode, the
transition/`chain_fraud` guests — sees a plain list and is unchanged. A
whole-`List` selection likewise yields the inline `0x02` bytes, not the wrapper.
Drafts inherit the format through one line in `runtime_tree_value`
(`DraftValue::List → ListHandle`); the incrementally-tracked draft root is
unchanged because the handle root *is* the list Merkle root.

The guests needed no source edits (they consume the format only through
`raster_core::input`); their zkVM image-ids regenerate on the next proving
build. Phase 2's remaining "removed elements" refinement (§edge cases: a
struct materializing a `List` field as its bare handle so a tile can read
`len()` but not elements) is still open — it is the storage-resolution change
this implementation deliberately avoided.

## Open questions

- **Arity and nesting**: `List<List<T>>` — the schema layer allows nested `List`
  nodes; does the surface? Proposed: yes for `List<List<T>>` (outer recur yields inner
  `List` references), no for `Block<Block<T>>` in v1 (a chunk of chunks has no CFS-
  pinned inner bound).
- **`Block` bound in the type?** `Block<T, const N: usize>` would carry the bound in
  the type, but ranges are start/end literals, not sizes, and const generics would
  infect every signature. Proposed: runtime-carried length, CFS-pinned bound —
  revisit only if a guest check needs the bound statically.
- **Maps**: `BTreeMap` fields have no `SchemaNode` today and stay unsupported; a
  future `Map<K, V>` node would follow the same pattern (referenced whole, entries
  selected/iterated).
