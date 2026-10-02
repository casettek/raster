# Issue: `program-output-unbound` — a sequence's returned value is not bound to its body, and `main`'s output can be any stored object

Status: open 2026-09-28. **Soundness gap in shipped code — mostly fixed** (2026-09-28): `ProgramEnd`
is held to the object `main` returns (`5e0bf83`) and to the part of it (`923aa9f`, which also
closed a zero-length-selection bypass), and every sequence's return is now followed statically
through the CFS, for `ProgramEnd` and for every argument taken from a sequence item. Still open: a
path through a sequence parameter (suffix only), two chains the walk does not follow, and the
object's own integrity (§What is fixed, and what remains).
Direction picked:
[`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md) §Sequence
return binding (decision D5c).

Related:
- [`program-end.md`](../proposals/program-end.md) (implemented) — §7 argues a forged output
  *"diverges from the fingerprint, which is fraud-provable"*. That holds only if the guest rejects
  the forged `ProgramEnd` step, which it does not (§What happens). A dated correction was added
  there 2026-09-28.
- [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) — adjacent, not the
  same. That issue is about the bytes a tile ran on versus the bytes its selection proves. This
  one is about *which object* a reference names at all.
- [`tile-output-commitment-unbound`](./tile-output-commitment-unbound.md) — what the fix here
  rests on. Pinning `ProgramEnd` to the coordinate a tile wrote pins whatever was *written* there;
  that issue is that nothing ties a tile's write to its replayed output.
- [`recur-carried-state-unbound`](./recur-carried-state-unbound.md) — the inline-state half of this
  gap, and the reason a recur site's stored result is not yet trustworthy as a program output.
- [`sequence-grammar-closure`](../proposals/sequence-grammar-closure.md) — the grammar rule that
  makes the fix single-valued: a sequence's only return form is *"last expression = a binding or
  call result"*, and control flow is forbidden in sequence bodies
  (`.claude/skills/raster/SKILL.md`, the sequence grammar table).

## What happens

A reference names a stored object by coordinates and commitment. The guest checks a reference as
precisely as the CFS describes where it came from:

| a step's argument comes from | what the CFS records | what the guest checks | precise? |
| --- | --- | --- | --- |
| a tile or recur tile, item `[j]` | "item `j`'s output" | coordinates **equal** `[j]` (`checks/cfs.rs`, the `Tile \| RecurTile` arm after `:742`) | yes |
| a sequence, item `[j]` | "item `j`'s output" | coordinates merely **inside** `[j]` (`checks/cfs.rs:741-744`, `has_coordinate_prefix`) | no |
| `main`'s return, at `ProgramEnd` | only `produces_output` (`raster-core/src/cfs.rs:696`) | the object is stored (`checks/entrypoint.rs:263`), the selection is valid (`:275`), the commitment is the selected hash (`:290`) | no |

A tile has one output, at its own coordinate. A sequence writes many objects under `[j]` — one
per tile it calls — and which one it returns is decided by its body, which the CFS does not
record: `SequenceDef` has no return binding, and the flow resolver resolves call *arguments*
(`raster-compiler/src/flow_resolver.rs:183`, `resolve_argument`) but never a body's returned
expression. `verify_program_end` (`checks/entrypoint.rs:223`) never consults the CFS beyond
`produces_output` (`:238`).

So a dishonest trace can substitute any object the sequence wrote:

```rust
#[sequence]
fn inner(list: …) -> AuthRef<Out> {
    let a = call!(prepare, list);                    // writes A at [2,1]
    let b = call_recur!(tile = t, input = list, …);  // writes B at [2,2]
    b                                                // returns B
}
#[sequence]
fn main(list: …) -> … {
    let r = call!(inner, list);    // item [2]
    call!(consume, r)              // item [3]: should read B
}
```

A trace in which `consume` cites A at `[2,1]` passes every check — the coordinates are inside
`[2]`, A is stored, the selection is valid, the replay's input equals the recorded input — and
claims `consume(A)`, a computation the program never performs. No step is invalid, so no fraud
proof can show it. `ProgramEnd` is looser still: it can name A, B, or any stored object.

## Reproduce

Measured 2026-09-28 with a throwaway guest test, added to the `program_end` module of
`crates/raster-prover/guests/transition/src/tests.rs` and removed afterwards. It uses that module's
own `producing_cfs()` — a `main` with **no items** — and stores one object at `[7]`, a coordinate
that names no CFS item:

```rust
let object_commitment = sha(b"some-intermediate-object");
let entry = StorageEntry { coordinates: CfsCoordinates(vec![7]), object_commitment: object_commitment.clone() };
let (_frontier, root, _index, index_root) = build_storage_context(&[entry.clone()]);
let witness = build_read_witness(&[entry.clone()], &entry);
let selected = sha(b"its-value");
let record = program_end_record(ProgramEndStep {
    output: Some(StorageData {
        coordinates: CfsCoordinates(vec![7]),
        commitment: object_commitment.clone(),
        selector: Default::default(),
        selection: SelectionCommitment {
            source_root_hash: object_commitment.clone().try_into().unwrap(),
            selected_hash: selected.clone().try_into().unwrap(),
            selected_len: 0,
            ..Default::default()
        },
    }),
    output_commitment: selected.clone(),
    storage: dummy_storage_roots(),
});
// verify_program_end(&producing_cfs(), &record, &program_end, &root, &index_root, Some(&witness), None)
```

Run with `cargo test zz_probe -- --nocapture` in `crates/raster-prover/guests/transition`. Result:
`Established { output_commitment: [218, 60, 174, …] }`.

The nested-sequence consumer case follows from `checks/cfs.rs:741-744` by reading; it was not
probed.

## Why it matters

`ProgramEnd`'s `Established` output is what a chain hands to the next program and what any
consumer of a "program completed" journal trusts (`program-end.md` §7, point 3). With the output
unbound, a committed trace can name an intermediate object — or any stored object — as the
program's result, and every step still verifies. The nested case lets a trace feed a step an
object other than the one the program routes to it.

A recur sequence's carried state is the same gap in inline form; that half is filed as
[`recur-carried-state-unbound`](./recur-carried-state-unbound.md).

## What it is not

- Not a selection-proof defect: every selection involved is valid. The object is simply the wrong
  one.
- Not `sequence-scope-forbids-narrowing`: that is a sub-sequence reading its *parameter*; this is a
  caller reading a sub-sequence's *result*.
- Not `trace-leaf-field-binding`: `ProgramEnd.output` is bound into the leaf and checked for
  internal consistency; what is missing is a check against the program.

## What is fixed, and what remains

**Fixed in `5e0bf83`: `ProgramEnd` names the object `main` returns.**

- *Build.* `raster-compiler` classifies `main`'s returned expression (`CallVisitor::classify_return`:
  the last value statement or a final `return`, looking through `Ok(..)`, `..?` and parentheses)
  and resolves it like an argument (`FlowResolver::resolve_return`): a tail call to the last item's
  output, a binding to its producing item or entry argument. The result is recorded as
  `SequenceDef.returns` for `main` only.
- *Guest.* `verify_program_end` requires `returns` (a missing one **fails closed**) and holds the
  output's coordinates to it (`verify_program_output_source`) with the same helper tile arguments
  use (`assert_prior_item_output_coordinates`): **equal** to `[j]` for a tile or recur tile,
  **inside** `[j]` for a sequence, `[]` for an entry argument; a data-sourced index is refused.

**Fixed after it (2026-09-28): the part of the object, and a proof bypass.**

- *The selector.* `returns` is now `SequenceReturn { source, path }`. The compiler records each
  `select!` alias's static path (`FunctionAstItem::selection_paths`, lowered as the `select!` macro
  lowers it) and composes the path along the returned binding's alias chain
  (`FlowResolver::compose_alias_path`) — the selector the runtime builds by appending each
  `select!`'s segments to its base's. An entry argument's path starts with the argument's name,
  as the runtime binds it (`entry_argument_auth_ref`). A returned `x.f` or `x[0]` written without
  `select!` is now *not* bound, rather than bound as the whole of `x`. The guest compares the path
  with `output.selection.path` — the path the selection proof is pinned to — not with `selector`,
  which no proof constrains: **exactly** for a tile's output or an entry argument, as a **suffix**
  for a sequence's output (the leading segments are the nested sequence's own selection).
- *A zero-length selection skipped its proof* (found while fixing the selector). A postcard-only
  object reports "no raster view" as an all-zero selection with `selected_len: 0`
  (`OwnedObject::resolve_whole`), and `verify_program_end` verified the selection proof only when
  `selected_len > 0`. So a `ProgramEnd` could claim `selected_len: 0`, keep `source_root_hash`
  equal to the object's commitment, and name any `selected_hash` as the program's output — the old
  test fixture did exactly that. The proof is now required unconditionally; an honest output is
  raster-encoded, and every raster payload has at least its tag byte. Step inputs have the same
  skip (`checks/store.rs:224`); it belongs to
  [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) and is not changed
  here.

*Tests.* The guest's `program_end` module builds a real raster object (`Stats { count, sum, max }`)
and genuine selection proofs: the `[7]` probe inverted, an intermediate object, another field, the
whole object for a field return, the zero-length bypass, another entry argument, and a nested
return with the wrong field are rejected; field, whole-object and entry-argument outputs are
accepted. `raster-compiler` tests the path lowering, alias composition and the entry-argument
prefix.

**In practice.** A `cfs` pass over all 15 programs (`examples/` and `raster-inference`): every
`main` return and every plain nested sequence's return binds, and every one is a whole object
(`path: []`) — no program returns a field, so the field-level cases are exercised by the tests
only. The only unbound returns are the three `finalize(draft)` mains below; the other sequences
without `returns` are recur-sequence bodies, skipped by design.

**Fixed after it (2026-09-28): nested returns, resolved statically.** The compiler records
`returns` for every value-returning sequence (`sequence_returns` in `cfs_builder.rs`; a
recur-sequence body is skipped — its site writes the result at its own coordinate). `returns` is
relative — an item index — because one definition is called from many places. The guest follows
it with `CfsCursor::resolve_value` (`raster-core/src/cfs.rs`) from `main`, or from any argument's
frame, down to the step that wrote the object: a tile, recur tile or recur-sequence site at its
own coordinate; through a nested sequence's `returns`, prepending its path; through a returned
parameter to the caller's argument; or to the entry object at `[]`. `ProgramEnd` requires the
output at exactly that coordinate (and fails closed if the chain cannot be followed); a
prior-item argument requires the same, falling back to the old "inside the source item" rule only
for the two chains the walk does not follow. On a real `hello-tiles` trace, `ProgramEnd` resolves
through three nested sequences to `exclaim`'s object at `[21,7,4,1]` — where the run put it — and
all 32 prior-item arguments resolve to the coordinates the run recorded. An honest sequence that
returns its own parameter, which the old rule refused, is now accepted.

**Remaining gaps.**

1. **A path through a sequence parameter is only a suffix**, and prior-item arguments get no path
   check. Argument bindings record no path, so once the walk crosses a parameter only the
   trailing segments are known. Closes with bindings that carry a path (the same change closes
   [`sequence-scope-forbids-narrowing`](./sequence-scope-forbids-narrowing.md)).
2. **Two chains are not followed**: a nested return the CFS could not bind (a nested
   `finalize(draft)`), and a recur-sequence body's parameter (the site source's element). A
   prior-item argument falls back to "inside the source item" for these; `ProgramEnd` fails
   closed. No program in `examples/` or `raster-inference` has either shape today.
3. **The object is only as good as its write.** A forged write at the right coordinate still
   verifies as the output: [`tile-output-commitment-unbound`](./tile-output-commitment-unbound.md).
4. **A recur site's result** is pinned exactly at `[s]`, but a site's stored result is not yet tied
   to its computation: the draft close check (`incremental-draft-materialization`) and
   [`recur-carried-state-unbound`](./recur-carried-state-unbound.md).

**Effects on existing programs.**

- **Programs returning `finalize(draft)` fail closed** at `ProgramEnd`: `finalize(d)` is a plain
  call, so the return cannot be bound, and the build prints a warning saying so. Confirmed by the
  `cfs` pass: `examples/chain-example/phase3-report`, and `raster-inference`'s `decode-init` and
  `decode-select-token` — exactly these three. `phase3-report` already fails earlier on the
  authenticated path ([`authenticated-chain-draft-output`](./authenticated-chain-draft-output.md)),
  and `finalize` leaves the language under `incremental-draft-materialization`. A build *warning*
  rather than the error the proposal specifies, so these programs still build and run
  unauthenticated.
- **A program's identity moves whenever its CFS changes** — with the first two changes to `returns`, every program with an output; with the third, only programs with nested sequences (`hello-tiles` among the examples). The CFS is
  postcard-encoded into `program_commitment` (`ProgramDefinition::canonical_bytes`), and postcard is
  not self-describing, so a new or reshaped field changes every program's bytes;
  `#[serde(default)]` does not help. An existing `program.bin` will not decode. The 6 tracked
  `Raster.lock` files in this repository are regenerated from from-scratch guest builds and pass
  `cargo raster program --verify`; the 9 in `raster-inference` are not. Regenerating exposed a
  separate problem: the tile guest build cache is keyed on the tile's source file and a build
  recipe, not on `raster-core` or `raster` — which every guest links, and whose changes do alter
  guest ELFs (guest builds are reproducible; `collect_line_chunk`'s image id moved with each of
  these changes) — so the cache served stale ELFs. Against the committed locks, 14 of the 29 tile
  image ids changed. **Fixed 2026-09-28**: `GuestBuilder::recipe_fingerprint` now also hashes the
  sources a guest compiles (`raster`, `raster-core`, `raster-macros`, the workspace manifest, and
  the user crate's lib, excluding `src/main.rs` and `src/bin/`), so a raster-only edit rebuilds and
  an unchanged tree is a cache hit. External crate versions remain unpinned: the guest resolves
  them fresh, with no lockfile.

## Directions

Picked, in `incremental-draft-materialization` §Sequence return binding: the CFS records each
sequence's return — its source binding **and its selector path** — resolved from the body's
returned expression by the same resolution as arguments, and followed statically through the CFS
by `ProgramEnd` and by every consumer of a sequence item. In place for storage-backed returns.
What remains: paths through parameters (gap 1, bindings with paths), the two unfollowed chains
(gap 2), inline returns (D5b), and the producers' own bindings (gaps 3 and 4).
