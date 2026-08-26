# Issue: `recur-accumulator-slots` — no loop-carried slot is both readable and incremental

Status: open 2026-08-25. Unowned.

Related:
- [`loop-carried-state.md`](../proposals/loop-carried-state.md) — **the closest neighbour, and it
  does not cover this.** It establishes the state/output asymmetry and proves the "an output
  draft is write-only inside the loop" half outright. Its subject is a *recur sequence* carrying
  a `List` by reference so a later round can `select!` it. This issue is about a *recur tile*,
  which has no `select!` and so cannot be served by a reference. §5 accounts for the boundary
  line by line.
- [`recur-sequence-break.md`](../proposals/recur-sequence-break.md) — owns "`call_recur_seq!`
  cannot stop early". §4 here is the sibling gap one level down: a `call_recur!` *can* stop
  early, and the thing it must consult to decide is the slot that costs the most to carry.
- [`incremental-draft-witness.md`](../proposals/incremental-draft-witness.md) — landed the
  frontier that makes `output` genuinely incremental. It is what turned this from "both slots are
  expensive" into a clean asymmetry: one slot got cheap, and it is the unreadable one.
- [`carried-state-channel.md`](../proposals/carried-state-channel.md) — if a state root ever
  becomes a real carrier, it is where it should live. Named for the implementer, not as a
  dependency.
- **`raster-inference` repo, `docs/issues/append-shaped-accumulators.md`** — the downstream half.
  Two of the loops originally cited here are not stuck on `state` at all; that issue owns the
  rewrites. §3 keeps only the loop that is genuinely stuck.

Motivating program: `raster-inference/raster-chain-inference`, stage `prefill-range`, tile
`accumulate_context`. §3 works it through and separates it from the matvec loops that merely
*look* like instances.

## 1. What the two slots actually are

A `call_recur!` site has two loop-carried slots, and they are built out of different materials.

**`state` is a value.** `RecurState<T>` holds `inner: T` by value
(`crates/raster/src/input.rs:102`) and exposes `get`, `get_mut`, `into_inner` (`:194`–`:202`).
The whole `T` is serialized into every iteration's `FnInput.values` as `FnInputValue::Inline`,
materialized into the tile, and re-serialized on the way out. Readable, writable at any offset,
and priced at `|T|` per iteration in both directions.

**`output` is a root.** `RecurOutput<S>` is `Draft<S>` (`:106`), and `Draft<S>` is
`{ anchor, current_root, [replay ops] }` (`:44`) — **it contains no `S` at all.** Its entire
public surface is `set_field` (`:509`) and `append_field` (`:518`), yielding `.set(v)` and
`.push(v)`. `SelectSource` is implemented for `TypedStorageBinding` (`:1039`) and `AuthRef`
(`:1073`) and nothing else, so a draft cannot be selected into either. Since
`incremental-draft-witness` landed, an iteration pays only for the increment it appends.

So the read-back gap is not a missing accessor. **There is no value to read.** A draft is a
commitment to a value under construction, and the construction is the only thing it holds.

The monotonicity that makes the root chain checkable is enforced in the guest, not by
convention — `apply_draft_ops` rejects a second `Set` on a field
(`crates/raster-core/src/draft.rs:944`) and admits `Push` only on `AppendOnlyVec` (`:952`).

## 2. The gap

An accumulator that survives a loop can need three things:

1. **read-back** — a bounds check, a `+=`, a "was this slot already written", a convergence test;
2. **incremental commitment** — per-iteration cost proportional to what the iteration *changed*,
   not to the accumulator's size;
3. **positional write** — landing a value at an index the iteration computes, rather than at the
   end.

| | read-back | incremental | positional write |
| --- | --- | --- | --- |
| `state` | ✅ | ❌ — whole `T` re-serialized and re-committed, both directions | ✅ |
| `output` | ❌ — holds no value | ✅ — frontier-based, pays the increment | ❌ — append-only by verification rule |
| `input` | ✅ (bounded window) | n/a — read-only, and it is the driving collection | ❌ |
| `args` | ✅ | ❌ — re-materialized identically every iteration | ❌ |

**No slot gives 1 and 2 together, and nothing gives 3 except the slot that fails 2.** An
accumulator that is bigger than a scalar and is written across iterations therefore has exactly
one legal home — `state` — and pays `2 · N · |T|` committed bytes to hold a `|T|` buffer across
`N` iterations, however little of it each iteration touches.

### The test for whether a loop is actually affected

A computed write index is not by itself a reason to need `state`. The question is narrower:

> **Does what the iteration writes depend on what is already there?**

If **no** — the slice is computed from `input` and `args` alone, and lands after everything
written so far — the loop is an *append*, and `output` serves it at increment cost. Any read of
the previous state in such a loop is **transport, not computation**: it exists only so the
iteration can re-emit the parts it did not touch, because it is the only party that gets to write
the buffer handed forward. An index that moves forward by a fixed step is an append with extra
arithmetic.

If **yes** — a `+=`, a running max, a rescale — `output` cannot serve at any offset shape, because
a draft is append-only by verification rule (`draft.rs:944`). That is the gap this issue names,
and applying the test is what separates the loops that have it from the loops that are merely
written as though they did. §3 does that for the motivating program.

This is not the unbounded-materialization problem `bounded-collections` closed. `|T|` here is
bounded, fixed at the top of the loop, and never grows. The amplification is in `N`, and `N` is
the number of iterations the program was decomposed into — so **the finer you bound the work per
replay unit, the more you pay to carry the accumulator through it.** The slot vocabulary
penalises exactly the decomposition the model asks for.

## 3. Where it bites, with numbers

> ⚠️ **A `### 3.1` subsection was lost here.** It was added to this file after I first read it and
> before I rewrote §3, and this file is untracked, so there is no copy. §7 still cites it as
> "§3.1 (0)" (`Fallible` accepted in no recur mode) and "§3.1 (3)" (the set-once guest rule).
> Restore it from your editor's undo history and re-anchor those two references.

Applying §2's test to every `call_recur!` tile in `raster-chain-inference` — five tiles, and only
two of them answer *yes*:

| tile | write depends on what's there? | verdict |
| --- | --- | --- |
| `mac_weight_page` (`prefill-prepare-aux/src/lib.rs:93`) | no — `out[i] = dot(..)`, disjoint rows, ascending | append-shaped; `state` is a choice, not a constraint |
| `score_key` (`prefill-range/src/lib.rs:405`) | no — `scores.push(..)`, literally an append | append-shaped; carrier *grows*, so the worst cost in the program is also the most fixable |
| **`accumulate_context`** (`prefill-range/src/lib.rs:519`) | **yes** — `*dst = mac_bits(*dst, w, v)` over every head × lane | **stuck; this section's subject** |
| `attend_kv_chunk` (`prefill-range/src/lib.rs:642`) | yes — a new head max rescales everything already accumulated | stuck, but **dead code** — no call site |
| `summarise_errors` (`prefill-range/src/lib.rs:1069`) | yes — reads `state.count` to decide `first` | correct as written — `ErrorSummary` is a `u32` and a `String`, bytes not kilobytes |

The first two are handed off to `append-shaped-accumulators` in the `raster-inference` repo. They
are cited here only as the contrast that makes the third one legible.

### The loop that cannot be rewritten

`prefill-range/src/lib.rs:519`, `accumulate_context` — the second pass of attention, folding each
visible key's value row into the context at its softmax weight.

```rust
let dst = &mut acc[head * head_dim + lane];
*dst = mac_bits(*dst, weight, *value);          // lib.rs:577
```

Every visible key adds into **every** head × lane. The write is a function of the accumulated
value, the touched region is the whole accumulator, and no offset trick converts it into an
append. Slot placement is correct: `input` is the real `List<KeyRow>` sweep, `args` are the query,
the weight vector and `LayerParams`, `state` is the only slot that can hold `acc`.

Shapes — `CtxAccum.acc` is `heads · head_dim` `i64`s (`zero_context`, `:501`), and the loop runs
over the whole key list per query row, twice (own keys and donor keys, `main.rs:99` and `:105`),
with the state chained across both:

```text
heads 8 · head_dim 256          |CtxAccum| = 8 × 256 × 8 = 16_384 B
N = |keys| + |donor_keys|       (the full list — a key that fails `visible()` returns
                                 state unchanged at :534 and still pays 2 · |T|)

useful writes   16_384 B once
carried state   2 · N · 16_384 B
```

At a 2048-key prompt that is **67 MB of committed state to maintain a 16 KiB buffer** — per query
row, per layer, before the donor pass doubles the iteration count. Roughly 4000×. Summed over the
query rows of one layer it is `O(L²)` in the prompt length, and the model has 35 of them.

### The contrast, and why it matters

`mac_weight_page` (`prefill-prepare-aux/src/lib.rs:93`) reads like the same problem and is not.
The matrix is 256 × 2048 × 4 = 2_097_152 B against a `PAGE_SIZE` of 196_608 B — 10 full pages of
24 rows plus one of 16, so 11 iterations over a 1 KiB accumulator:

```text
useful writes        256 slots × 4 B                 =   1_024 B
carried state        11 iterations × 2 × 1_024 B     =  22_528 B     22× amplification
```

22× looks like a small instance of the same gap. It is not an instance at all. Line 134 unpacks
all 256 slots and line 141 writes 24 of them with `=`, not `+=` — the unpack is transport, and the
loop is an append wearing a computed index. Rewritten output-only it costs 1_056 B, and the 22×
disappears without any change to the slot vocabulary.

**The distinction is the point.** An issue that counts every expensive `state` carrier as evidence
overstates itself, because most of them are cheap to fix in the program. What is left after the
program is fixed — `accumulate_context`, and `attend_kv_chunk` if it is ever revived — is the
actual gap, and it is the larger cost of the two.

These are arithmetic from declared shapes, not a benchmark; the `heads = 8` / `head_dim = 256`
figures are Gemma 3n E2B and should be confirmed against the imported `config.json`. The real
numbers live in `TileProfileRecord.input_bytes` / `output_bytes` behind `raster-runtime`'s
`profiling` feature (`crates/raster-runtime/Cargo.toml:26`), and the amplification claim should be
confirmed there before anyone designs against it — §7.

The workaround does not exist. There is no way to say "carry this buffer, materialize only the
window this iteration touches": `input` is taken by the driving collection and admits one
collection per site, `args` is re-materialized whole, and `output` cannot be read. For
`accumulate_context` the window is the whole buffer anyway, which is §6's problem.

## 4. The corollary: an early exit is decided on the expensive slot

**All three recur-tile modes accept `RecurControl`** — `raster-macros/src/recur.rs:141`
(output-only), `:167` (state-only), `:200` (state+output). Early exit is not a state+output
privilege.

But `RecurControl` is a decision, and a decision needs something to read. In output-only mode
the tile can consult `input` (the current item), `args` (constants), and iteration position
(`RecurInput::index`/`len`/`is_first`/`is_last`, `crates/raster/src/input.rs:172`–`:184`). It
cannot consult a single byte of what it has built. So the only breaks expressible output-only
are *item-local* ("this element is the sentinel") and *positional* ("stop at 100"). Every
accumulated predicate — converged, budget spent, target found and already recorded, error
latched — is out of reach.

`mac_weight_page` is the example, and it is the one the model *does* support:

```rust
// state-only, legal today (recur.rs:167) — and not what the program does:
) -> RecurControl<RecurState<ProjAccum>> {
    if !state.error.is_empty() {
        return RecurControl::Break(state);   // stop; skip the remaining pages entirely
    }
    // …

// the same intent, output-only — unwritable:
) -> RecurControl<RecurOutput<ProjVec>> {
    // there is no expression here that can name a failure from an earlier
    // iteration: `input` is this page, `args` are constants, and `output`
    // holds no value. The control type is available; the predicate is not.
    RecurControl::Continue(output)
}
```

The tile as written short-circuits to a no-op tail instead (`lib.rs:97`), which produces the
right answer and still materializes and commits every remaining 192 KiB page. That is a missed
`Break`, not a gap — §5's side finding covers why the documentation made it look mandatory. The
gap is the second block: **a loop that builds into `output` can hold `RecurControl` and can
never form a reason to use it.**

Recovering it means adding `state`, which lands back in §2: the loop pays `2 · N · |T|` for a
carrier whose only job is to hold the break condition.

And the carrier it lands on is the weaker one. `recur-progress-commitment` binds the control
bit itself — a trace claiming `Continue` where the replay journal says `Break` is rejected
(that proposal's §Tests, "the control bit is bound, not trusted") — so the guest proves *the
tile returned `Break` given these bytes*. It does not prove those bytes were the previous
iteration's output: a `RecurState` travels as `FnInputValue::Inline`, and the guest's entire
obligation for an inline binding is that it *is* inline
(`crates/raster-prover/guests/transition/src/checks/cfs.rs:371`). `loop-carried-state` §Problem
establishes the substitution; the consequence it does not draw is this one:

> When the break condition is computed from `state`, an unchecked state carrier does not merely
> corrupt the accumulated value — **it lets the prover choose the trip count**, with rule 6 of
> `lazy-list-recur` (a `Break` permits an incomplete prefix, `covered_end <= L`) admitting the
> short sweep as legitimate.

That is the same failure class as the "committed counter list" the authoring skill names: the
number of iterations becomes an input rather than a consequence. Here it arrives through a slot
the program was pushed into using by §2, rather than through a fixture someone wrote by hand.

Whether this is severe depends on whether `loop-carried-state`'s by-reference state lands first;
it is recorded here because today no document connects the two halves.

## 5. What is already owned, and where the line falls

`loop-carried-state` §"Why `output` cannot serve" proves point 1 of §2's table decisively, for a
different program and a different conclusion. The boundary:

| | `loop-carried-state` | this issue |
| --- | --- | --- |
| loop form | `call_recur_seq!` — a sequence per item | `call_recur!` — a tile per item |
| carried thing | a `List<T>` that a later round must re-read | a fixed-size buffer read and rewritten in full each step |
| why it cannot be carried | `List` is not `Materializable`, so it cannot be in `state` at all | it *can* be in `state`, and is; the cost is the problem |
| shape of the fix | pass a root, `select!` it next round | — none proposed; `select!` does not exist inside a recur tile |
| motivating program | `raster-tokenizer` BPE merge, unrolled 48× | `raster-chain-inference` attention context accumulation |

The decisive difference is the last row of the middle column. A by-reference `state` is readable
because the *sequence* body can `select!` it. A recur **tile** body is plain Rust with no
selection surface — it receives materialized values by construction (authoring skill §3) — so
handing it an `AuthRef` would require materializing it anyway. `loop-carried-state`'s fix is
correct for its problem and is a no-op for this one.

Nothing else covers it. `draft-provenance` is about a draft losing its binding at `finalize`,
after the loop. `bounded-collections` made unbounded materialization unrepresentable and says
nothing about a bounded value materialized `N` times. `incremental-draft-witness` fixed the
write side and thereby created the asymmetry.

### Side finding: the authoring skill states §4's premise backwards

`.claude/skills/raster/SKILL.md:458` lists early stop as "state+output step returning
`RecurControl`", and `references/recur.md` presents `RecurControl<(RecurState<S>,
RecurOutput<O>)>` as "the ONLY early-exit mechanism in the model". Both are wrong against
`raster-macros/src/recur.rs:141`/`:167`/`:200`, which accept `RecurControl` in all three modes.

The practical cost is that a program with a state-only fold and a failure to latch — like
`mac_weight_page`, which short-circuits to a no-op tail on error (`lib.rs:97`) rather than
breaking — reads as correctly written against the documented model. It is not: it could `Break`,
and the pages after the failure are materialized for nothing. The skill fix is independent of
this issue and should not wait for it.

## 6. Directions, none chosen

Sketches, with the objection each has to answer. Picking one is a proposal's job.

1. **A readable draft — a `get`/`len` surface on `Draft<S>`.** Answers: the draft holds no
   value, only a root and an op journal, so a read is a reconstruction; and a guest that must
   reconstruct to verify has lost the property `incremental-draft-witness` bought.

2. **A third slot: a fixed-size, positionally-writable carrier committed by increment.** A
   `RecurBuffer<T>` whose per-iteration transition is a sparse patch (index, value) chained like
   `DraftReplayTransition`. Answers: `apply_draft_ops`' monotonicity is what makes the chain
   checkable (`draft.rs:944`), and an overwrite is exactly what monotonicity forbids — so this
   needs its own verification rule, not a relaxation of that one. It is also a fourth carrier,
   which `carried-state-channel` §Problem argues against on its own terms.

3. **Windowed state — declare that iteration `i` materializes only `state[f(i)]`.** Answers:
   `f` must be authorized rather than tile-chosen, which is the `dynamic-index-selection` argument
   applied to a write; and a window function pinned in the CFS is a real expressiveness cut. Note
   what it no longer answers: it fits the *matvec* exactly (`f(i) = page.offset() / stride`), and
   §3 established that the matvec does not need a new slot at all. For `accumulate_context` the
   window is the whole buffer, so `f` is the identity and the direction buys nothing.

4. **Nothing — declare the amplification acceptable and document the ceiling.** Answers: §3's
   4000× is per query row per layer across 35 layers, and the numbers are unmeasured. This
   direction is only available after the profiling run, not before.

**What §3 does to this list.** The test narrowed the affected set to loops with a dense
read-modify-write, and that is the shape directions 2 and 3 were sketched *against the wrong
example* for. Direction 2's sparse patch is not sparse when every lane is touched; direction 3's
window is not a window when it is the whole buffer. Both were sized for `mac_weight_page`, which
turns out to need neither. Only direction 1 (a readable draft) addresses `accumulate_context`, and
its objection — that reconstructing to read gives back what `incremental-draft-witness` bought —
is the hardest of the four. A fifth shape may be needed: a carrier that is committed by increment
in *one* direction (the iteration's contribution) while the accumulated value is reconstructed only
where a verifier asks for it. Nobody has sketched that here.

Directions 1 and 2 do not address §4; direction 3 does, by making the break condition cheap
enough to keep in `state`. None of the four makes `state`'s carrier checked — that is
`loop-carried-state` plus `carried-state-channel`, and §4 is a reason to sequence them ahead of
whichever direction is chosen.

## 7. Reproducing

The cost claim is arithmetic over declared shapes and can be read off the source:

```bash
# §2's test, tile by tile — two answer yes, three answer no
sed -n '570,582p'   raster-inference/prefill-range/src/lib.rs   # *dst = mac_bits(*dst, — stuck
sed -n '705,745p'   raster-inference/prefill-range/src/lib.rs   # rescale on new max    — stuck, dead
sed -n '128,148p'   raster-inference/prefill-prepare-aux/src/lib.rs  # out[i] = dot(..) — append
sed -n '427,450p'   raster-inference/prefill-range/src/lib.rs   # scores.push(..)       — append
sed -n '1069,1081p' raster-inference/prefill-range/src/lib.rs   # reads state.count     — correct

# the accumulator shapes and the call sites
grep -n -A6 'struct CtxAccum\|struct ScoreAccum' raster-inference/prefill-range/src/input.rs
grep -n 'call_recur!' raster-inference/prefill-range/src/main.rs
grep -rn 'attend_kv_chunk' --include=*.rs raster-inference/ | grep -v /target/   # no call site

# the two slots, mechanically
sed -n '44,50p;95,110p;194,204p;505,525p' crates/raster/src/input.rs

# RecurControl in all three modes — and `Fallible` in none of them
grep -n 'RecurControl' crates/raster-macros/src/recur.rs
grep -n 'Fallible'     crates/raster-macros/src/recur.rs   # no hits: §3.1 (0)

# set-once, the guest rule behind §3.1 (3)
sed -n '940,955p' crates/raster-core/src/draft.rs
```

Measured figures, which this issue does not have. `profiling` is a `raster-runtime` feature
(`crates/raster-runtime/Cargo.toml:26`), not a CLI flag; with it enabled `cargo raster run`
writes `profile.json` / `profile.ndjson` into `target/raster/runs/<run_id>/`
(`crates/raster-cli/src/commands.rs:49`–`:53`; there is no `latest` pointer for runs):

```bash
cargo raster run --input input.json --input-manifest input_manifest.json
cargo raster analyze target/raster/runs/<run_id>/profile.json
```

For `accumulate_context` the state carry per iteration is `input_bytes` minus the `KeyRow` and
the args, and `output_bytes` is the whole `CtxAccum`. Both should be flat across the key sweep;
that flatness *is* the issue. For `score_key` both should climb linearly, which is the
`raster-inference` half — the same profile run distinguishes them.
