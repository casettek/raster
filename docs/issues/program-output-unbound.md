# Issue: `program-output-unbound` — a sequence's returned value is not bound to its body, and `main`'s output can be any stored object

Status: open 2026-09-28. **Soundness gap in shipped code.** Direction picked, not implemented:
[`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md) §Sequence
return binding (decision D5c), with the `ProgramEnd` half to land first as a standalone fix.

Related:
- [`program-end.md`](../proposals/program-end.md) (implemented) — §7 argues a forged output
  *"diverges from the fingerprint, which is fraud-provable"*. That holds only if the guest rejects
  the forged `ProgramEnd` step, which it does not (§What happens). A dated correction was added
  there 2026-09-28.
- [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) — adjacent, not the
  same. That issue is about the bytes a tile ran on versus the bytes its selection proves. This
  one is about *which object* a reference names at all.
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

## Directions

Picked, in `incremental-draft-materialization` §Sequence return binding: the CFS records
`SequenceDef.returns: Option<InputBinding>`, resolved from the body's returned expression by the
same resolution as arguments, with an unresolvable return a build error. `SequenceEnd` records its
returned value as a binding, as `ProgramEnd` already does; the guest checks both against `returns`;
a consumer of a sequence item cites exactly the returned binding. `ProgramEnd` first: `main`'s
`returns` and one check in `verify_program_end`, with the probe above inverted as its test.
