# Proposal: `clone-free-bindings` — a sequence binding is a coordinate; its consumers should borrow it

Status: Proposed 2026-09-24. **Design not chosen** — this document states the problem, argues why
the fix is safe, and costs four options. The recommendation at the end is a first cut, not a
decision.

Related:
- [`sequence-grammar-closure`](./sequence-grammar-closure.md) — §What this does not fix explicitly
  leaves `.clone()` *inside* a `select!` path out of scope. This is that scope. Phase 1 of that
  proposal introduced `clone!` as the written form for a standalone clone; this one asks whether
  most of the remaining clones need to be written at all.
- `.claude/skills/raster/SKILL.md:373` — documents the status quo: "Inside a `select!` path a bare
  `.clone()` is still the spelling."

## Problem

`select!` binds its base once and then **moves** it (`crates/raster-macros/src/lib.rs:3754`):

```rust
let __raster_select_base = #base_expr;          // moves `draft_greeting`
::raster::select_source(__raster_select_base, …)
```

because `SelectSource::select(self, …)` takes `self` by value (`crates/raster/src/input.rs:1031`).
So any base read more than once has to be cloned, and the call sites confirm the clone is a move
artifact rather than an intent: the *last* read of a base never carries one.

```rust
let draft_title      = select!(String, draft_greeting.clone().title);   // clone
let first_draft_line = select!(String, draft_greeting.lines[0]);        // no clone
```
(`examples/hello-tiles/src/main.rs:103-104`, and the same shape at
`crates/raster/tests/draft_selection.rs:83-85`.)

Measured across `crates/` and `examples/`: **37 `select!` bases, 6 `call!`-family arguments and 6
`storage!` references** carry a `.clone()`. Three consumers produce all of them —
`select!`, `call!` / `call_recur!(input = …)`, and `storage!(T, r)`, whose
`typed_storage::<T>(r)` also takes its `StorageRef` by value (`crates/raster/src/lib.rs:623`).

The cost is ergonomic *and* pedagogical. A reader new to Raster sees `.clone()` beside an
authenticated value and reasonably infers that cloning does something to lineage. It does not.

## Why removing it is safe

1. **Cloning an `AuthRef` has no meaning for verifiability.** It copies a `StorageRef`, a
   `SelectorPath`, an index-binding `Vec` and an `Rc` (`input.rs:914-944`) — the same coordinate,
   no new commitment, no new lineage. It is a real allocation, not a free one, so the clones are
   not even paying for themselves.
2. **It is not a linearity mechanism.** `AuthRef` already derives `Clone`, so nothing is enforced
   by the move. Contrast `Draft` (`input.rs:44`), which deliberately is *not* `Clone` and *is*
   threaded linearly — that consumption is real and this proposal does not touch it.
3. **The CFS does not see the clone.** `expr_root_ident` strips `Expr::MethodCall` through its
   receiver (`crates/raster-compiler/src/ast.rs:664`), so `draft_greeting.clone().title` and
   `draft_greeting.title` resolve to the same root. Removing clones is provenance-neutral by
   construction, not by argument — and it *shrinks* the surface
   `sequence-grammar-closure` phase 2 has to close, since the `.clone()` arm at `ast.rs:835`
   exists only to keep the pre-DSL spelling from falling through to `Inline`.

## Options

### 1. `select!` borrows its base

Emit `let __base = &(#base_expr);` — temporary-lifetime extension covers a `storage!(…)` base —
and add `impl SelectSource for &AuthRef<C>` / `&TypedStorageBinding<R>` that clone internally.

```rust
let draft_title = select!(String, draft_greeting.title);
```

- ~40 lines. One wrinkle: `emit_index_lit` / `emit_range_lit` splice `&#base_ident`
  (`lib.rs:3487`, `:3516`), which would become `&&AuthRef<T>`; drop the extra `&` there.
- **Backward compatible** — existing `.clone()` sites keep compiling (they clone twice,
  harmlessly), so the call-site sweep is mechanical and can land separately.
- Fixes only `select!`; the `call!` and `storage!` clones remain.

### 2. Option 1 applied to every consumer

Same move for `call!` arguments, `call_recur!(input = …)`, and `storage!` (take
`impl Borrow<StorageRef>`, which also kills `stored.reference().clone()` —
`reference()` already returns `&StorageRef`, `input.rs:858`).

- Harder part is `call!`: arguments are heterogeneous (`"Rust".to_string()` beside an `AuthRef`),
  so it needs an autoref-friendly conversion — a blanket `impl IntoAuthRef<T> for &AuthRef<T>` —
  rather than a bare `&`. `clone!` already has this shape: it takes `&$binding` and clones inside
  (`lib.rs:693`).
- Removes essentially every `.clone()` from a sequence body. This is the complete version of 1.

### 3. Make `AuthRef` a `Copy` handle

`AuthRef<T>` becomes `{ id: u32, PhantomData<T> }` over a per-run arena — the shape `Draft`
already uses ("live draft handle backed by thread-local runtime state", `input.rs:40`).

- **Zero macro changes**: `Copy` turns every existing move into a copy, so all three consumers are
  fixed at once and old `.clone()` calls still compile.
- Costs: the `Inline(T)` variant must be boxed into the arena (`Box<dyn Any>`, `T: 'static` — which
  the `SelectSource` impls already require, `input.rs:1041`); the arena must work in both the std
  host and the `no_std` guest posture; nothing is dropped during a run; `Debug` resolves through
  the arena.
- Conceptually the most honest — an `AuthRef` *is* an id, not data — and the deepest change.

### 4. Generated lenses instead of a macro

Have `#[derive(Selectable)]` emit `&self` accessors (it already emits a `{Ident}DraftExt` trait):

```rust
let title = draft_greeting.title();                            // AuthRef<String>
let slice = person.addresses().at(0).lines().range::<0, 2>();  // Block<String>
let row   = table.rows().at_bound(&wanted);                    // bound index
```

- No clone, no repeated type name, real completion, and the rules `select!` enforces with
  `compile_error!` (range ⇒ `Block`, `BytesPage` needs an index, `lib.rs:3760-3785`) become
  type-level.
- Costs: the largest derive change; separate spellings for ranges, `.pages`, byte offsets and bound
  indexes; two ways to say the same thing during migration. It is an ergonomics axis, orthogonal to
  1–3, and would layer on any of them.

## Recommendation

Option 1, then Option 2. Together they are small, backward compatible, and remove the large
majority of the clones without touching the guest. Option 3 is the better end state if an arena in
the guest is acceptable; Option 4 is a separate question about whether selection should be a macro
at all, and should not ride in on this.

Migration, if 1+2 is taken:

1. Land the borrow in `select!` with existing call sites untouched (they still compile).
2. Sweep `crates/` and `examples/` — mechanical, and `commit.bin` must stay byte-identical, since
   by §Why removing it is safe (3) the CFS cannot see the difference. That byte-identity is the
   acceptance test for the whole proposal.
3. Extend to `call!` / `call_recur!` / `storage!`.
4. Update `SKILL.md:373` and §4's grammar table, and drop the `.clone()` arm at `ast.rs:835` —
   coordinate with `sequence-grammar-closure` phase 2 rather than duplicating it.

## What this does not change

- **`Draft` stays linear.** Never cloned, never reused after being passed (`SKILL.md:441`).
- **`clone!` stays.** An explicit re-binding of a reference is still a legitimate form; this
  proposal only removes the clones that exist to satisfy the borrow checker.
- **No change to what is committed.** No selection, commitment, trace event or program identity
  moves. If any of them do, the change is wrong.

## Open questions

- Does anything depend on `select!` consuming its base — e.g. a diagnostic that relies on
  use-after-move to catch a stale binding? Nothing found, but the sweep would prove it.
- Should Option 2's `call!` conversion be a blanket impl on `&AuthRef<T>` or an explicit
  `as_auth_arg` helper? The blanket impl is invisible at the call site, which is the goal, but it
  also makes the argument grammar harder for `sequence-grammar-closure` to enumerate.
- Is `Copy` (Option 3) worth revisiting once the arena exists for another reason? It subsumes 1 and
  2 entirely, so landing them first is only wasted work if 3 arrives soon.
