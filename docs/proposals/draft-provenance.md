# Proposal: `draft-provenance` — `finalize` is a narrowing, and `Inline` needs a positional rule

Status: Proposed (2026-07-30).
Related: [`sequence-grammar-closure.md`](./sequence-grammar-closure.md) — this
refines that proposal's classification table and answers its "Should `Fresh` be
narrower?" open question. It is not an alternative to it: closure is still the
right end state, and this document argues closure alone would *ratify* one
existing hole rather than close it.
[`dynamic-index-selection.md`](./dynamic-index-selection.md) — the `into_ref`
incident is the sibling instance of the same class.
Motivating program: `raster-inference/raster-tokenizer`, a Gemma tokenizer whose
`call_recur_seq!` **input** — the collection being iterated — resolved to
`Inline`.

## Problem

`finalize(draft)` severs a provenance chain the resolver otherwise tracks
correctly.

The chain is tracked. In `examples/hello-tiles/src/main.rs` the draft is threaded
through three tiles, and the CFS binds every link:

```text
[5] Tile set_draft_greeting_title   [Inline, Inline]                 <- new!(T), genuinely fresh
[6] Tile push_draft_greeting_line   [Inline, PriorItemOutput{5}]     <- draft from item 5
[7] Tile push_draft_greeting_line   [Inline, PriorItemOutput{6}]     <- draft from item 6
[8] Tile concat_messages            [Inline, Inline]                 <- ??
```

Item 8's two arguments are `draft_title` and `first_draft_line`, both `select!`ed
out of `finalize(draft)`. They should bind to `PriorItemOutput{7}` — the tile
that last wrote the draft. Instead the resolver reports that the object this
program just built through three authorized tile calls has **no upstream**.

The mechanism is the one `sequence-grammar-closure` describes:
`CallVisitor::expr_root_ident` (`crates/raster-compiler/src/ast.rs:598`)
recognizes paths, fields, indexes, method calls and the `select!` / `clone!` /
`into_ref!` macros. `finalize(d)` is an `Expr::Call`, which falls through to
`_ => None`, so the local is unattributed and `resolve_argument` returns
`InputBinding::inline()` (`crates/raster-compiler/src/flow_resolver.rs:207`).

What makes this worth its own document is that `sequence-grammar-closure`'s
Phase 2 would not fix it. That proposal's surface table lists

| form | resolves to |
| --- | --- |
| `new!(T)` / `storage!(T, r)` / **`finalize(d)`** | `Inline` — genuinely new |

and its open questions propose to "allowlist `finalize` by name and reject other
calls". Both make the current behaviour *intentional*. Closing the grammar
against unrecognized forms while classifying `finalize` as fresh would leave
this hole open, now with a specification behind it.

## Why `finalize` is not fresh

`finalize` consumes a `Draft<S>` and returns an `AuthRef<S>` to the value that
draft built (`crates/raster/src/input.rs:1936`). It reads a root the draft
already accumulated, resolves it to a `StorageRef`, and hands back a reference.
It creates no data.

That is the definition `sequence-grammar-closure` gives for `Narrowing`:

> `into_ref!` and `clone!` — hand back a reference to the same data […] Neither
> narrows, computes, or produces a step, so the result carries exactly the
> provenance of its argument.

`finalize` satisfies all three: no new data, no step in the CFS (there is no
finalize item in any sequence's item list), and its result *is* what the
argument names. The three `Fresh` forms are not alike:

| form | creates | correct classification |
| --- | --- | --- |
| `new!(T)` | an empty draft, nothing upstream | `Fresh` |
| `storage!(T, r)` | a typed view of an explicit ref | escape hatch (see below) |
| `finalize(d)` | **nothing — reads `d`** | **`Narrowing` over `d`** |

The runtime already agrees. In the motivating program the recur whose input was
`finalize`-derived ran fine: `call_recur!` demands a storage-backed list and
errors otherwise (`crates/raster/src/input.rs:1289`), and the finalized draft
*is* storage-backed. So runtime provenance was real while schema provenance was
absent — and the schema is the half that gets committed into program identity.
**A divergence between what the runtime can resolve and what the schema records
is the general signature of this bug class.**

## Goal

1. `finalize` carries its draft's provenance, so a value built across tiles binds
   to the tile that built it.
2. `Inline` in a recur `input =` slot becomes impossible to express, not merely
   unlikely.
3. `sequence-grammar-closure` Phase 2 can reject *every* `Expr::Call` without an
   allowlist.

## Surface

`finalize!(draft)` — a macro, replacing the function. No other syntax changes.

```rust
let draft = call!(push_draft_greeting_line, line, draft);
let greeting = finalize!(draft);                    // binds to the push above
let title = select!(String, greeting.clone().title);
```

This is exactly the `into_ref!` remedy, applied to the one piece of the grammar
that never received it. That precedent is explicit
(`crates/raster/src/lib.rs:582`):

> **A macro rather than a method** […] Keeping this a macro means the only
> spelling available is the one the analysis understands.

## Mechanism

### 1. `finalize` joins the macro grammar

- `crates/raster/src/input.rs`: rename `finalize` to `__raster_finalize`,
  `#[doc(hidden)]`. Body unchanged.
- `crates/raster/src/lib.rs`: add the macro beside `into_ref!`, expanding to
  `$crate::__raster_finalize($draft)`. The prelude already re-exports `finalize`
  next to the macros, so import paths are unaffected.
- `crates/raster-compiler/src/ast.rs`: add `finalize` to `is_reference_macro`,
  so `reference_macro_root` supplies the alias.

Under `sequence-grammar-closure`'s `SequenceForm`, `finalize` moves from `Fresh`
to `Narrowing(root)`.

### 2. No `Expr::Call` is ever recognized

Once `finalize` is a macro, the allowlist that proposal contemplates is
unnecessary: `Unrecognized` can cover **every** call expression. That is a
strictly simpler rule than "allowlist `finalize` by name", and it removes the
maintenance hazard the allowlist would create — a second place to remember when
adding surface.

This is the answer to that document's open question: `Fresh` should be narrower,
and the way to narrow it is to stop `finalize` being a call at all.

### 3. A positional rule: a recur input may never be `Inline`

`sequence-grammar-closure` scopes argument-position `Inline` out, correctly, on
the grounds that `call!(f, 42)` is sanctioned configuration. That reasoning does
not extend to every position. The `input =` slot of `call_recur!` /
`call_recur_seq!` is *by definition* a committed collection: the runtime rejects
anything else at `input.rs:1289`. There is no literal a program could
legitimately iterate.

So the CFS build should reject a recur item whose input binding is `Inline`,
independent of whether the general closure work has landed. This is cheap — one
check over `RecurTileItem` / recur-sequence items — and it is the check that
would have caught the motivating program at rung 2 instead of by hand.

Note it is a *positional* invariant, not a form rule: it holds whatever the
resolver's form coverage is, which is why it is worth having in addition to
closure rather than instead of it.

### 4. `storage!` is the escape hatch, and should be named as one

`storage!(T, reference)` legitimately produces an unattributed binding — it is
how tests and scaffolding inject a storage ref that the sequence did not
compute. It is also the one sanctioned way to feed a recur input that has no
in-sequence upstream (`crates/raster/tests/recur_draft.rs`, `dynamic_index_selection.rs`).

Rule 3 must therefore exempt `storage!`, and that exemption should be explicit
rather than incidental. This makes `storage!` the concrete answer to
`sequence-grammar-closure`'s proposed `inline!` escape hatch, at least for
collections: the hatch already exists, it is already greppable, and it already
says what it means. Whether `inline!` is still needed for scalars is left to
that proposal.

## Evidence

**`hello-tiles`.** Items 5–8 above. The bug is in the tree's canonical example,
has been for as long as drafts have existed, and survived Phase 1.

**`raster-inference/raster-tokenizer`.** A tile built the prompt's derived list,
which was finalized and fed to `call_recur_seq!`:

```rust
let windows_draft = call!(atomize_prompt, prompt, max_special_len, new!(PromptWindows));
let windows = finalize(windows_draft);
let window_list = select!(List<String>, windows.windows);
let probes = call_recur_seq!(sequence = probe_position, input = window_list, ...);
```

`window_list` resolved to `Inline`: the collection the loop iterates, pinned to
nothing. Because a tile cannot return a `List` (it is not `Materializable`, per
[`bounded-collections.md`](./bounded-collections.md)), **draft + `finalize` is
the only way a tile can produce a collection** — so this is the normal path for
any program that derives a list, not an unusual one.

That program worked around it by committing the list as an entry argument
instead. That is a real cost the fix removes: it pushed a decomposition into the
fixture generator to satisfy the resolver, not the model.

## What this does not fix

- **It does not verify the alias is right.** `finalize!(d)` aliasing to `d` is
  checkable by construction, not proven — the same caveat `clone!` carries.
- **It does not make `Inline` safe in argument position generally.** Rule 3
  covers recur inputs only. Other positions where `Inline` is never legitimate
  may exist; each needs its own argument.
- **It does not close the grammar.** That is `sequence-grammar-closure`'s Phase
  2, and it remains necessary: this document removes one misclassification and
  adds one positional check, neither of which makes the resolver total.

## Edge cases

- **`finalize!(new!(T))`** — the root is `new!`, not a narrowing macro, so it
  resolves to `Inline`. Correct: an untouched draft has no upstream.
- **A draft threaded from a sequence parameter** (`RecurSequenceOutput`) roots at
  `SequenceScope`, unchanged.
- **`raster::finalize!(d)`** — `macro_path_is` already matches `name!` and
  `raster::name!`.
- **Program identity.** Changing a binding from `Inline` to `PriorItemOutput`
  changes the CFS and therefore `program_commitment`. Every program with a draft
  re-locks once. This is drift in the direction of a *stronger* schema, but it is
  drift, and `cargo raster program --verify` will report it.

## Migration

1. **`finalize!` macro + resolver alias**, with the existing
   `test_reference_macros_alias_to_their_argument` extended to cover it. Migrate
   the five in-tree call sites: `examples/hello-tiles/src/main.rs:101` and four
   in `crates/raster/tests/draft_selection.rs`.
2. **Rule 3**, the recur-input check, with a negative test. Independent of step
   1 and of `sequence-grammar-closure`; landing it first would have caught the
   motivating program.
3. **Fold into `SequenceForm`** when `sequence-grammar-closure` Phase 1 lands:
   `finalize` is a `Narrowing` constructor, and `Expr::Call` maps to
   `Unrecognized` with no allowlist.
4. **Re-lock** affected programs and record the identity change.

## Open questions

- **Should `storage!` also be a narrowing over its ref?** Its argument is often a
  binding, and treating it as one would tighten the escape hatch. Argument
  against: its purpose *is* to introduce a ref the sequence did not compute, so
  narrowing it would be misleading in exactly the case it exists for. Proposed:
  leave it `Fresh` and exempt it explicitly in rule 3.
- **Are there other misclassified forms?** This document found one by running a
  real program and reading the dump. The general lesson is that
  `sequence-grammar-closure` closes *unrecognized* forms but nothing checks that
  each form classified `Fresh` genuinely creates data. A positive test per
  `Fresh` constructor — "this form has no argument that is a binding" — would
  make the second failure mode as unrepresentable as the first.
- **Should the recur-input rule be a proc-macro diagnostic instead?**
  `call_recur!` sees its own `input =` expression, so it could reject a
  non-grammar form at rustc time without the shared crate that
  `sequence-grammar-closure` step 3 needs. That may be a cheaper first
  diagnostic than the general one.
