# Issues — gaps without an owner

Index of `docs/issues/`. Last reviewed 2026-08-25.

An **issue** names a gap. A **proposal** owns a design for closing one. The two are kept
apart because the failure mode they guard against is different: a proposal that opens with a
weak problem statement is hard to argue with, because the design is already on the table and
the reader is arguing against work someone did. An issue is the problem statement alone,
written before anyone is invested in an answer.

An issue must:

- **be reproducible from the code as it stands** — every claim cited to a file and line, or to
  a program in `raster-inference` that hits it;
- **say what it is not** — which existing proposal is adjacent, and precisely where that
  proposal's scope stops short of this;
- **stop before the design.** A §Directions section may sketch shapes and name their costs. It
  must not pick one. When a direction is picked, it graduates to `docs/proposals/` and the issue
  becomes a `Closed by:` line.

An issue that cannot yet be reproduced is a note, not an issue; keep it in the relevant
proposal's §Uncertainties instead.

## Open

| issue | opened | subject | adjacent proposals |
| --- | --- | --- | --- |
| [`recur-accumulator-slots`](./recur-accumulator-slots.md) | 2026-08-25 | No loop-carried slot is both readable and incrementally committed. `state` is readable and re-committed whole every iteration; `output` is incremental and unreadable. A loop whose write is a function of the accumulated value — §2's test — has no cheap home, and a data-dependent `Break` can only be decided from the expensive one. Narrowed 2026-08-25: only 2 of `raster-chain-inference`'s 5 recur tiles pass the test; the rest are program-side, see that repo's `append-shaped-accumulators`. | [`loop-carried-state`](../proposals/loop-carried-state.md), [`recur-sequence-break`](../proposals/recur-sequence-break.md), [`incremental-draft-witness`](../proposals/incremental-draft-witness.md) |

## Closed

_None yet._
