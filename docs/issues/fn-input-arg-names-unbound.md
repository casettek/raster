# Issue: `fn-input-arg-names-unbound` — which storage binding is argument `i` is the host's choice

Status: **resolved 2026-10-19** — §Resolution. Opened the same day at `c049b76`, by reading, and
reproduced by test before the fix. Found while mapping the data the CFS checks read, for
[`control-flow-prover`](../proposals/control-flow-prover.md), whose step 1 closes it.

Related:
- [`control-flow-prover`](../proposals/control-flow-prover.md): extracts the CFS checks into
  their own guest. Every check this issue concerns moves into that guest, so the fix is that
  proposal's step 1.
- [`sequence-scope-forbids-narrowing`](./sequence-scope-forbids-narrowing.md) (resolved
  2026-10-08): the same comparison (`assert_scope_source`), a different defect. That issue was
  about the comparison being too strict about paths. This one is about which two values are
  compared in the first place.
- [`tile-io-structural-roots`](../proposals/tile-io-structural-roots.md) step 2: its
  `verify_input_roots` is what pins argument names on a **tile** step's own witness (§Why tile
  steps are partly covered). It does not reach the scope witness.

## What happens

A step's input witness is a `FnInput` (`raster-core/src/trace.rs:26`):

```rust
pub struct FnInput {
    pub data: Vec<u8>,
    pub values: Vec<FnInputValue>,   // positional: Inline(bytes) | StorageBinding
    pub args: Vec<FnInputArg>,       // positional: { name, ty }
    pub storage: StorageInput,       // BTreeMap<name, StorageData>
}
```

`FnInputValue::StorageBinding` carries no name. The only link from position `i` to a storage
binding is `args[i].name`, which is used as a key into `storage`:

- `checks/cfs.rs:36` `resolved_source_at`: every CFS binding check resolves argument `i` this way,
  for the step's own witness **and** for the caller's (`cfs.rs:888`, the `SequenceScope` arm).
- `checks/cfs.rs:93` `recur_sequence_item_source`, `cfs.rs:1104`, `cfs.rs:1246`, `cfs.rs:1288`,
  `cfs.rs:1355`: recur checks, same lookup.
- `checks/io.rs:165` `verify_input_roots`, at `io.rs:192`.

The record commits to the witness through `input_source_commitment`, and that commitment does not
include `args`:

```rust
// raster-core/src/trace.rs:67, used by the recorder (raster-runtime/src/tracing/recorder.rs:267)
// and the guest (checks/io.rs:16)
pub fn source_witness_bytes(&self) -> Vec<u8> {
    postcard::to_allocvec(&(self.values.clone(), self.storage.clone())).unwrap_or_default()
}
```

So for any two storage-bound positions, a host can supply `args` in either order, or name the same
binding twice, and the witness still hashes to the recorded commitment.

## Why tile steps are partly covered

On a tile step, `verify_input_roots` (`io.rs:165`) compares `replay.input_roots[i]`, which the tile
guest commits positionally, with the selection root of binding `args[i].name`. A permutation of the
step's **own** `args` therefore has to match the order the tile actually received its arguments
in, so the replay pins it. That holds only where a replay exists and only for bindings with
`selected_len > 0` (`io.rs:202`).

Nothing pins:

1. **The scope witness's `args`.** The scope witness is the caller's `FnInput`. Its hash is checked
   against the parent `SequenceStart`'s `input_source_commitment` (`cfs.rs:719`), which does not
   include `args`. The parent step's own check ran in a different guest execution, against
   whatever `args` were supplied *then*. Nothing makes the two the same.
2. **The own `args` of every non-tile step.** `SequenceStart`, `RecurStart`, and recur-sequence
   iterations carry no replay.

## What it allows

Take a sequence `s(p: T, q: T)` whose item `t` is a tile declared with
`[SequenceScope{0, []}, SequenceScope{1, []}]`. Honestly, `t` runs on `(A, B)`, where `A` and `B`
are `s`'s two storage arguments.

A challenger fabricates the window's last record `t'`, in which the tile ran on `(B, A)`:

| check | what the challenger supplies | passes because |
| --- | --- | --- |
| `io::verify_step_record`: replay | a real tile receipt for input `(B, A)` | it is a real execution of the registered tile |
| `verify_input_roots` | `t'`'s own `args` in honest order, with storage `{x: B, y: A}` | `input_roots[0] = root(B)` = the root of binding `x` |
| `checks::store` | read proofs for `A` and `B`, and a write of `t'`'s output | both objects exist, and the write slot is empty |
| `verify_sequence_scope_parent` | `s`'s `FnInput` with `args` reordered to `[q, p]` | `values` and `storage` are unchanged, so the hash still equals the parent's commitment |
| `verify_one_binding`, arg 0 | consumer `x → B` against caller position 0 → `q → B` | equal |
| `verify_one_binding`, arg 1 | consumer `y → A` against caller position 1 → `p → A` | equal |

`t'` diverges from the honest committed record, and every check passes. That is the `Finished`
condition, and it gives a fraud receipt against an honest trace. Naming one binding twice
(`args = [p, p]`) works the same way for a step that reads `A` twice instead of `(A, B)`.

Preconditions: one item binds two (or more) of its caller's storage parameters, with compatible
declared paths (empty, or equal), and the tile can decode the swapped input. In practice that
means same-typed parameters, which is the common case of a sub-sequence forwarding two `List<T>`
or two config objects.

`EntryArgument` and `PriorItemOutput` bindings are **not** exposed in the same way: the first is
checked against `selection.path`, which starts with the argument's name, and the second against
`StorageData.coordinates`. Both are inside the commitment. A swap between them fails those
comparisons. What remains exposed is every chain of `SequenceScope` bindings, back to the first
frame whose arguments are pinned by one of those two.

## What it is not

- Not [`selection-unbound-from-execution`](./selection-unbound-from-execution.md). That issue
  was about the bytes a tile ran on not being tied to the bytes its selection proves, and
  `verify_input_roots` closed it. Here every selection and every root is genuine. Only the
  positions are swapped.
- Not a resolver or CFS defect. The CFS declares the right binding per position. The guest
  compares the right binding with the wrong caller value.

## Directions

Not picked here; [`control-flow-prover`](../proposals/control-flow-prover.md) step 1 picks one.

1. **Commit `args` names.** `source_witness_bytes` hashes `(values, args names, storage)`. This is
   one shared function, so the recorder and the guest change together. Every
   `input_source_commitment` changes, so every trace leaf and every `commit.bin` fixture changes.
2. **Put the name in the value.** `FnInputValue::StorageBinding(name)`. `values` is already
   committed, so the position-to-binding map becomes part of the commitment, and `args` is used
   only for display. This changes the `FnInput` type and every place that matches on it: 14
   outside tests, across the guest, `raster-runtime`, `raster-macros` and `raster`.
3. **Put parameter names in the CFS.** `TileDef` and `SequenceDef` record parameter names, and
   the guest asserts `args[i].name == declared[i]`. This changes program identity, not trace
   commitments. It covers named sequences, but recur-sequence iteration frames name their item
   by a marker (`cfs.rs:73`), not by a declared parameter.

## Reproduction

By reading: the four citations above (`trace.rs:67`, `cfs.rs:36`, `cfs.rs:719`, `cfs.rs:888`).

The test that would show it, in the guest's `tests.rs` style: build a two-parameter sequence
fixture as in §What it allows. Call `verify_sequence_scope_parent` and then
`verify_step_record_inputs` with the scope witness's `args` reversed and the consumer's storage
swapped to match. Today both return. After the fix, `verify_sequence_scope_parent` must panic on
the commitment comparison.

**Reproduced 2026-10-19**, before the fix, by the two `should_panic` tests in §Resolution: both
reported "test did not panic as expected". The reordered (`[q, p]`) and the duplicated (`[p, p]`)
caller witnesses passed `verify_step_record_inputs` and `verify_sequence_scope_parent`.

## Resolution

Direction 1: the names are committed. `FnInput::source_witness_bytes`
(`raster-core/src/trace.rs`) now hashes `(values, arg names, storage)`. The recorder
(`recorder.rs:267`) and the guest (`checks/io.rs:16`) both hash that one function's output, so
they cannot disagree. `ty` stays out (it is `stringify!` output, which is not stable across
compilers, and nothing checks it), and so does `data` (it is committed separately, as
`input_commitment`).

A reordered or duplicated name list now hashes to something other than the parent
`SequenceStart`'s recorded commitment, so `verify_sequence_scope_parent` refuses it. The same
change pins the step's **own** names on non-tile steps, through the anchor in
`io::verify_step_record`.

Tests, in the transition guest's `tests.rs`:
- `a_scope_witness_with_reordered_arg_names_is_refused`
- `a_scope_witness_naming_one_binding_twice_is_refused`
- `the_honest_two_parameter_scope_witness_verifies` (positive control)

Cost: every `input_source_commitment` changes, so traces and `commit.bin` files recorded before
the change no longer verify against the new guests. Every tile image ID moved as well (tile
guests compile `raster-core`), so all six tracked `Raster.lock` files were regenerated: hello-tiles
(24 of 24 IDs changed) and the five chain-example stages (all of their IDs changed). The tracked
`examples/hello-tiles/fraud_commit.bin.fraud-proof` is stale and was left as is.
