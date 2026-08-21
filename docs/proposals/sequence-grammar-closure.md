# Proposal: `sequence-grammar-closure` — an unrecognized sequence form must not silently mean "unbound"

Status: Proposed (2026-07-30). Phase 1 (`clone!`, `into_ref!`, and the
`.clone()` backstop) is implemented; this document is about Phase 2, inverting
the default.
Related: [`dynamic-index-selection.md`](./dynamic-index-selection.md) — the
`into_ref` incident described there is one instance of the class this proposal
closes. [`draft-provenance.md`](./draft-provenance.md) — a *second* instance,
found after Phase 1: it argues the `finalize(d)` row of the surface table below
is misclassified, and answers the "Should `Fresh` be narrower?" open question. [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) —
the sequence grammar currently lives there as prose; this makes one copy of it
executable.

## Problem

The CFS flow resolver decides where each step argument came from by walking the
sequence body's AST. When it recognizes a form, it binds the argument to that
form's provenance. When it does **not** recognize a form, it falls through to
`InputBinding::inline()` (`crates/raster-compiler/src/flow_resolver.rs:208`).

`Inline` is not a neutral answer. It means "this argument has no upstream", and
the codebase already states what that costs
(`flow_resolver.rs:578`):

> Binding it as `Inline` would let a claimed trace substitute arbitrary bytes
> for it and still verify.

So the resolver's fallback for *"I don't understand this"* is the same as its
answer for *"this is a literal"* — and the two cases have opposite security
meanings. Unrecognized is the permissive direction.

This is not hypothetical. `RecurSequenceInput::into_ref()` shipped on
2026-07-30 as the surface the `dynamic-index-selection` proposal's appendix
specified. It was correct at runtime and correct in its proof; it was simply a
form `visit_local` did not match, because that function only recognized
`select!`. The result was that `let id = input.into_ref();` produced

```json
{ "Direct": "Inline" }
```

for both the id and the `BoundIndex` citation taken from it — an unauthenticated
index, arriving through the very API built to make indexes authenticated. It was
found by reading a CFS dump by hand, not by any test or diagnostic.

The same hole applied to `binding.clone()` — a form the sequence grammar
*explicitly sanctions* — for as long as it has existed. Phase 1 fixed both
instances. Neither fix addresses the mechanism that produced them.

### Why this keeps happening

The grammar is enforced in three places that do not agree and are not derived
from each other:

| where | what it knows | how it fails |
| --- | --- | --- |
| `SKILL.md` table | the full grammar, in prose | not executable |
| `#[sequence]` proc macro | a few forms (rejects computed `select!` indexes) | silent about the rest |
| `CallVisitor::visit_local` | `select!`, `into_ref!`, `clone!`, `.clone()` | **silently permissive** on anything else |

Adding surface to the DSL means remembering to teach the third one. Forgetting
is not a build error, a test failure, or a warning — it is a weaker schema.

## Goal

Make the flow resolver **total** over sequence bodies: for every `let`
initializer, either it is a form the grammar defines and the resolver attributes
it, or it is rejected. Remove `Inline` as the answer to an unrecognized form,
while keeping it as the answer to a genuine literal.

## Surface

No new user-facing syntax. The grammar is exactly what `SKILL.md` §4 already
documents:

| form | resolves to |
| --- | --- |
| `call!` / `call_seq!` / `call_recur!` / `call_recur_seq!` | `PriorItemOutput` |
| `select!(T, path)` | source's binding, `Indexed` if the path has a bound index |
| `into_ref!(handle)` / `clone!(binding)` | argument's binding |
| `new!(T)` / `storage!(T, r)` / `finalize(d)` | `Inline` — genuinely new |
| a literal | `Inline` — genuinely new |
| **anything else** | **rejected** |

The change users see is a new diagnostic, not new syntax.

## Mechanism

**One list, one owner.** A single `SequenceForm` enum in `raster-compiler`
classifies an initializer expression, and both the resolver and the diagnostic
read from it. Today the knowledge is spread across `is_selection_macro`,
`is_reference_macro`, `macro_call_kind`, and an `Expr::MethodCall` arm in
`visit_local`; those become constructors of the one enum.

```rust
enum SequenceForm {
    Step(CallKind),        // call!, call_seq!, call_recur!, call_recur_seq!
    Narrowing(String),     // select!, into_ref!, clone! — carries the root ident
    Fresh,                 // new!, storage!, finalize(), literals
    Unrecognized,          // -> diagnostic
}
```

`resolve_argument` keeps its current behaviour for the first three. The fourth
stops being `Inline`.

**Where the diagnostic fires.** Two options, and they are not exclusive:

1. **`#[sequence]`, at rustc time.** Best feedback — ladder rung 1, with a span
   pointing at the offending `let`. Precedent exists: `select!` already rejects
   computed indexes with UI tests under `crates/raster/tests/ui/`. Cost: the
   proc macro needs the form list, so it must live in a crate both
   `raster-macros` and `raster-compiler` depend on.
2. **`cargo raster cfs`, at rung 2.** No shared-crate refactor, but later
   feedback and easy to skip.

Recommended: (1) as the diagnostic, (2) retained as the backstop, so a program
that somehow reaches the compiler with an unrecognized form fails there rather
than being bound to nothing.

**Escape hatch.** Some program will eventually have a legitimate reason to bind
something the grammar does not cover. Rather than reintroducing silence, that
should be explicit — `inline!(expr)`, which resolves to `Inline` and *says so*.
A reviewer can then grep for it. Whether to ship this in v1 or wait for a real
motivating case is open.

## What this does not fix

- **Argument-position `Inline` stays legitimate.** `call!(f, 42)` is sanctioned
  configuration and must keep working. This proposal is about `let`
  initializers, where there is no equivalent case.
- **It does not verify the aliases are *right*.** `clone!(x)` aliasing to `x` is
  a claim this proposal makes checkable-by-construction but not proven; a
  narrowing macro that lied about its root would still lie. Keeping the
  narrowing set tiny is the mitigation.
- **It does not close the selector-path grammar.** `.clone()` inside a
  `select!` path is handled by `split_selector_expr` and is out of scope.

## Migration

1. **Introduce `SequenceForm`** and reroute the existing predicates through it.
   Pure refactor; no behaviour change, no identity change.
2. **Add the `cargo raster cfs` diagnostic** for `Unrecognized`. Run the ladder
   across `raster-examples/` — the grep below says nothing in-tree should break,
   which makes this the cheap step to land first.
3. **Lift the diagnostic to `#[sequence]`**, with UI tests per rejected form.
   This is the step that needs the shared crate.
4. **Decide on `inline!`** once a real case appears.

### Blast radius

Measured 2026-07-30 across `raster/crates/` and `raster-examples/`: **zero**
`let`-bound initializers in sequence bodies that the grammar does not already
cover. The 21 `let x = y.clone();` sites found are all in runtime internals,
CLI, tests, and host-side executors — not sequences. So steps 1–2 are expected
to be behaviour-preserving on every program in the tree.

That number is also the argument for doing this *now* rather than later: the
cost only grows.

## Open questions

- **Should `Fresh` be narrower?** `finalize(d)` is an `Expr::Call`, and treating
  every unrecognized `Expr::Call` as `Fresh` rather than `Unrecognized` would
  re-open a smaller version of the same hole. Proposed: allowlist `finalize` by
  name and reject other calls, consistent with how `clone` is allowlisted.
  **Superseded by [`draft-provenance.md`](./draft-provenance.md)**, which argues
  `finalize` is a `Narrowing` rather than `Fresh` at all — measured against
  `hello-tiles`, whose draft chain the resolver tracks through items 5→6→7 and
  then drops at item 8. Making it a `finalize!` macro removes the need for an
  allowlist: no `Expr::Call` need ever be recognized.
- **Does the proc-macro diagnostic need the full CFS?** It only needs to
  classify forms, not resolve bindings — so the shared crate can be small
  (the enum plus the macro-name predicates). Worth confirming before committing
  to the split.
- **Warn or error, in step 2?** A warning lets the ecosystem migrate; an error
  is what the security argument actually calls for. Given the measured blast
  radius is zero, error seems affordable immediately.
