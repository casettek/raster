# Proposal: `unauthenticated-execution` — run a Raster program as plain Rust, with authenticated storage off

Status: **v1 implemented** (2026-08-19)

Implementation notes, where the code corrected the design:

- **A storage-backed `AuthRef` is not mode leakage** (§5.4). `main`'s declared parameters are
  bound as `AuthRef::Storage` in *both* modes (`entry_argument_auth_ref`,
  `crates/raster/src/input.rs:791`) because they come from committed files rather than from a
  tile. The unauthenticated arm resolves them, which also keeps external inputs lazy. The mode is
  about values passed *between tiles*; it was never about the program's boundary.
- **`storage!` works** rather than being the limitation §5 predicted, for the same reason.
- **Drafts and recur are refused at first use, not at startup** (§7) — a runtime cannot know
  which constructs a program will reach. Still before any draft or iteration work happens.
- **`Bytes::pages` became a public field** (`crates/raster-core/src/collections.rs`), because
  `bytes_schema` already advertises `pages` as a struct field and generated accessor code reaches
  it by that name. Read-only in practice: the sibling fields stay private.
- **No test needed changing**, as §1 predicted. `force_auth_mode` was added for the one new test
  binary that wants the non-default mode.
Related:
- `docs/specs/core/3-execute/01-runner-modes.md` — names four runner modes (native, audit,
  zkVM-preview, window replay). All four keep authenticated storage; this adds a posture
  *below* all of them. §2 explains why it is a posture and not a fifth mode.
- [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) (half landed) — owns
  RAS-203 and RAS-206. §9 shows RAS-206 already rules out the classic divergence sources, and
  proposes the one amendment to RAS-203 this needs.
- [`bounded-collections.md`](./bounded-collections.md) (implemented) — owns `List<T>` / `Block<T>`.
- [`dynamic-index-selection.md`](./dynamic-index-selection.md) (phases 1–3 implemented) — owns
  "a `select!` index must be an authorized value". §5.3 states the one deliberate weakening.
- [`zkvm-dry-run.md`](./zkvm-dry-run.md) (proposed) — **naming conflict**: it claims
  `cargo raster run --dry-run` for RISC0 executor replay. This proposal uses `--no-auth`.

## Naming

The mode is `AuthMode::Unauthenticated`, opposite `AuthMode::Authenticated`.

`Native` was considered and **cannot be used**: it is taken twice over, by `BackendType::Native`
/ `--backend native` and by `01-runner-modes.md` §"Mode: native", where it already means
"execute without a zkVM" — a program can be native *and* authenticated, which is today's
default, so reusing the word would make the two most common postures indistinguishable.

`Unauthenticated` is chosen over `Plain` / `Direct` / `Lightweight` because it names what is
absent rather than how the run feels, and in a project whose product is verifiability the mode
that cannot be verified should say so in its own name. The CLI spelling is `--no-auth`.

## Problem

Authenticated storage is not optional, and it is not free.

Every tile output in a sequence goes through `bind_infallible_call` / `bind_fallible_call`
(`crates/raster/src/lib.rs:434`, `:458`), which postcard-encodes the value, SHA-256-hashes it
(`internal_object_commitment`, `crates/raster-runtime/src/storage.rs:223`), appends it to
storage, and returns a handle. Every tile *input* runs the inverse: resolve the binding, read
the bytes back, decode to `T`, and hand the tile a plain value.

Each inter-tile value therefore costs a serialize, a hash, a store, a load and a deserialize.

**Who consumes that cost.** The storage commitment is not only read by `--commit`. It is
recorded in the trace itself: `FnInput.storage` is a map of `StorageData { coordinates,
commitment, selector, selection }` (`crates/raster-core/src/trace.rs:30`, `:37`), one entry per
storage-bound tile argument. So the moment a trace is being written, every commitment has a
reader. (The hash itself is computed once at store time by `internal_object_commitment`; the
trace copies it rather than recomputing it. Tile *outputs* are traced as raw bytes — `FnOutput`
at `:243` carries `data`/`ty`/`raster`, no commitment — with output coordinates published
separately.)

That gives three postures, not two:

| run | storage commitment is… |
| --- | --- |
| `cargo run` — no trace | computed and **read by nothing** |
| `cargo raster run` — trace written, not committed | recorded in `StorageData.commitment`, then printed as step records and discarded |
| `cargo raster run --commit` / `--audit` | recorded, and folded into the trace commitment / recomputed and compared |

Only the first row is pure waste, and it is the row this proposal is about. The second row is
weak consumption — the commitment is written down but nothing ever checks it — and this proposal
does **not** touch it: a run that produces a trace keeps producing an authenticated one.

This matters for more than accuracy. Because §6 makes unauthenticated mode emit no trace at all,
"the mode is unauthenticated" and "no consumer exists for the commitment" are the *same
condition*, not two conditions that happen to coincide. The proposal never switches off storage
that something was reading.

The three other pieces of Raster machinery are already switchable, and switch off by *absence*:

| machinery | gate | plain `cargo run` |
| --- | --- | --- |
| trace capture | `RASTER_TRACE_PATH` (`crates/raster-runtime/src/tracing.rs:77`) | off |
| output artifact | `RASTER_OUTPUT_DIR` (`crates/raster-runtime/src/input.rs:2673`) | off |
| profiling | `RASTER_PROFILE_*` + a cargo feature | off |
| **authenticated storage** | **none** | **on, always** |

The consequence is visible today. Running `examples/hello-tiles` with a bare
`cargo run -- --input input.json --input-manifest input_manifest.json` writes no trace, no
profile and no artifact — and still prints `AuthRef { storage: "storage", commitment_len: 32, … }`
for every binding, because every one of them was hashed and stored on the way through.

That is the wrong default for the two things people spend most of their time doing: iterating on
a program's logic, and reading its output.

## Goal

A **runtime** posture in which `AuthRef` carries values directly instead of storage handles, so
a Raster program executes as plain Rust: tiles, sequences and `select!` still run and still
compose, but nothing is serialized, hashed, stored or resolved between them.

Because it is a runtime posture, the same binary runs either way, and the posture is decided at
the moment the run is launched — which is where the interlock belongs.

Stated up front:

- **Unauthenticated runs cannot produce a trace commitment.** Not "should not": cannot. §6 makes
  this structural rather than a check.
- **Not a build-time feature.** An earlier draft proposed a cargo feature, mirroring the existing
  `#[cfg(all(feature = "std", not(target_arch = "riscv32")))]` split in
  `crates/raster-macros/src/lib.rs:2439`/`:2544`. Rejected: the mode is a property of the run,
  not of the artifact, and a build-time switch puts the mode in a different place from the
  decision to commit. §5.1 shows the runtime flag costs nothing in `select!` lowering, which was
  the only real argument for the feature.
- **`Draft<S>` and the recur drivers are out of scope for v1.** §7.
- **Chain-level policy is out of scope entirely.** §10 records the direction and why it is a
  separate proposal.

## Facts the design builds on (verified in code)

- **`AuthRef` already has the variant this needs.**
  `pub enum AuthRef<Current> { Inline(Current), Storage(DeferredAuthStorage<Current>) }`
  (`crates/raster/src/input.rs:843`). `AuthValue<T>` has the same split with a working
  `into_inner()` (`crates/raster-core/src/input.rs:1336`, `:1353`) and an `as_storage()` that
  already returns `None` for the value-carrying variant (`:1360`).

  The existing variant is spelled `Inline`, which is also the name of an unrelated concept —
  `FnInputValue::Inline` in the trace, and `InputSource::Inline` in CFS flow resolution, both
  meaning "an argument with no upstream binding". The variant name stays as-is (renaming it
  would touch the trace vocabulary and the prover guest); the *mode* is `Unauthenticated`, and
  §11 lists keeping those two distinct in prose as a documentation requirement.
- **Exactly one place refuses the value-carrying variant.** `SelectSource for AuthRef`
  (`crates/raster/src/input.rs:1064`) panics at `:1080`: *"select! on inline sequence values is
  not supported; use committed storage bindings instead"*. That panic is the substance of this
  proposal.
- **Tile bodies already only ever see plain `T`.** `gen_auth_value_materialization`
  (`crates/raster-macros/src/lib.rs:438`) ends every parameter's materialization with
  `let #name: #value_ty = __raster_auth_value.into_inner();`. The authenticated layer lives
  entirely *between* tiles. **No tile body changes.**
- **Two write sites, no others.** `bind_infallible_call` and `bind_fallible_call`
  (`crates/raster/src/lib.rs:434`, `:458`). Each already has a `#[cfg(not(feature = "std"))]`
  sibling, so "return an `AuthRef` without storing" is a shape the crate already expresses.
- **`select!` has the path structurally at expansion time.** `split_selector_structured`
  (`crates/raster-macros/src/lib.rs:3238`) decomposes `pd.addresses[1].lines[0]` into a base
  expression plus ordered segments before any runtime type is involved; `emit_selector_segments`
  (`:3360`) lowers them to `SelectorSegment` values. The information needed to emit a direct
  field access instead is present in the same function.
- **`List<T>` and `Block<T>` are already plain newtypes.** `pub struct List<T>(Vec<T>)`
  (`crates/raster-core/src/collections.rs:108`), `Block<T>(Vec<T>)` (`:199`), with
  `impl<T> From<Vec<T>> for List<T>` at `:145`.
- **CFS generation does not execute the program.** `commands::cfs`
  (`crates/raster-cli/src/commands.rs:596`) builds from `CfsBuilder` — source analysis only.
  **Program identity is untouched by this proposal**: `cargo raster program --verify` produces
  the same commitment whether or not this mode exists.
- **External input commitments are checked in one place.** `FileInputSourceResolver`
  (`crates/raster-runtime/src/source/file.rs:66`–`:95`).
- **`--commit` never reaches the process.** Handled host-side in
  `crates/raster-cli/src/commands/run.rs:258`–`:270`, after the child exits, over the trace
  file. This is what makes §6 work.

## Design

### 1. The mode and where it comes from

```rust
pub enum AuthMode { Authenticated, Unauthenticated }
```

Resolved **once** per process, lazily on first use, cached in a `OnceLock`.

**`Authenticated` is the default everywhere. Exactly one thing lowers it:** a program entered
through a `#[sequence] main`, with `RASTER_AUTH` unset.

```
RASTER_AUTH=1 / =0 set          → Authenticated / Unauthenticated   (always wins)
otherwise, raster::init() ran   → Unauthenticated
otherwise                       → Authenticated
```

The seam is exact, and it already exists. `raster::init()` is called from precisely one place:
the `fn main() { ::raster::init(); … }` that the `#[sequence]` macro generates for a program
entry point (`crates/raster-macros/src/lib.rs:3027`–`:3028`). Nothing else in the codebase calls
it. Every other way into the runtime — `init_with(publisher)`
(`crates/raster-runtime/src/tracing.rs:101`), or driving `SequenceScopeGuard` / `store_value`
directly — leaves the default untouched.

That makes the three postures fall out without anyone having to opt in:

| | mode | why |
| --- | --- | --- |
| `cargo run` on a Raster program | **Unauthenticated** | went through `init()`, no `RASTER_AUTH` |
| `cargo raster run` | **Authenticated** | CLI sets `RASTER_AUTH=1` |
| `cargo test`, library embedding | **Authenticated** | never reaches `init()` |

**`cargo test` is always authenticated, and that is a requirement, not a convenience.** Raster's
tests exist to check that authentication is *correct*; a test suite that silently ran with
authentication switched off would assert nothing about the thing it was written to assert. This
rule holds for all nine storage-exercising files under `crates/raster/tests/` with no change to
any of them — four already enter via `init_with` (`dynamic_index_selection`, `paged_bytes`,
`external_selection`, `recur_draft`), and the rest never call an init at all.

`cargo raster run` sets `RASTER_AUTH=1` alongside the env it already sets at
`crates/raster-cli/src/commands/run.rs:148`–`:161`, and `RASTER_AUTH=0` when `--no-auth` is
passed. `cargo raster chain run` sets `RASTER_AUTH=1` (see §10).

Two consequences worth stating:

- `init()` is public API, so a test that calls it directly opts that binary into unauthenticated
  execution. `RASTER_AUTH=1` overrides, and the rule is documented (§11) rather than enforced.
- Caching matters: the mode must not read as two different values within one run, or a sequence
  could store half its bindings and pass the rest directly. Resolving on first use and caching in
  a `OnceLock` is what prevents a late `RASTER_AUTH` change from splitting a run.

### 2. Why this is a posture, not a fifth runner mode

`01-runner-modes.md` defines native / audit / zkVM-preview / window replay as *how* a program is
executed. This is orthogonal to all four: it is native execution with the authentication layer
removed. It cannot combine with audit, zkVM-preview or window replay — those require exactly the
bindings it removes — so it composes only with native, and the spec change is a paragraph in
§"Mode: native", not a new section.

### 3. Write side

```rust
pub fn bind_infallible_call<T>(result: T) -> AuthRef<T>
where T: Serialize + DeserializeOwned + 'static
{
    match auth_mode() {
        AuthMode::Unauthenticated => AuthRef::Inline(result),
        AuthMode::Authenticated => { /* exactly as today */ }
    }
}
```

`bind_fallible_call` takes the same shape, with the `Ok`/`Err` split applied to the value
directly rather than through `resolve_storage_ok_value`.

The `T: Serialize + DeserializeOwned` bounds stay. They are unused in the unauthenticated arm,
but keeping them means a program that compiles one way compiles the other — which is what makes
"develop unauthenticated, commit authenticated" a workflow rather than a trap.

### 4. Read side

`into_auth_value_with_bindings` returns `AuthValue::Inline(value)` with no resolution step.
`as_storage()` already returns `None` for that variant, so `gen_auth_value_materialization`'s
existing `#internal_info_ident` binding (`crates/raster-macros/src/lib.rs:492`) becomes `None`
with no codegen change, and `.into_inner()` yields the value as it does today. **Close to free.**

### 5. `select!` — the load-bearing part

#### 5.1 Dual lowering

The macro emits both arms and branches on the cached mode:

```rust
// select!(String, personal_data.name)
match ::raster::auth_mode() {
    Authenticated  => { /* compose SelectorPath, defer — exactly as today */ }
    Unauthenticated => AuthRef::Inline(personal_data_value().name.clone()),
}
```

The unauthenticated arm is a real field access, not a runtime path-walk:
`split_selector_structured` already yields the segments, so re-emitting them as Rust accessors
on the unwrapped value is mechanical. This is why the runtime flag costs nothing here — the
branch is on a process-lifetime value and predicts perfectly, and both arms are monomorphic.

| `select!` | unauthenticated arm |
| --- | --- |
| `select!(String, pd.name)` | `pd.name.clone()` |
| `select!(List<Address>, pd.addresses)` | `List::from(pd.addresses.clone())` |
| `select!(Address, addresses[1])` | `addresses[1].clone()` |
| `select!(Block<T>, xs[a..b])` | `Block::from(xs[a..b].to_vec())` |
| `select!(EmbeddingRow, rows[token_id])` | `rows[token_id as usize].clone()` |

#### 5.2 `BytesPage`

Page selection exists because bytes are paged *in storage* (`paged-bytes.md`, rev 3 implemented;
`BytesPage` at `crates/raster-core/src/collections.rs:815`). Unauthenticated, a region is a
`&[u8]` and a page index is an offset into it. `select!(BytesPage, region[i])` lowers to
constructing a `BytesPage` over the corresponding slice using the type's declared `page_size`.

This is the newest surface and the likeliest to resist; it is built first for that reason.

#### 5.3 Indices lose their authorization — deliberately

`IndexSource` has no blanket impl for plain integers, because an unauthorized index is exactly
what `dynamic-index-selection.md` exists to prevent. Here that restriction protects nothing:
there is no lineage to break. So the unauthenticated arm accepts a plain integer index.

**This is a real weakening, confined to this mode.** A program that is only ever run
unauthenticated can use an index the authenticated mode rejects, and will fail the first time it
is run for commitment.

#### 5.4 A storage-backed base is legitimate, and resolves

Two `select!` bases are storage-backed in *both* modes, and neither is mode leakage:

- **`main`'s declared parameters.** `entry_argument_auth_ref`
  (`crates/raster/src/input.rs:791`) always builds an `AuthRef::Storage`, because an external
  input comes from a committed file, not from a tile.
- **`storage!(T, reference)`**, which names a storage coordinate outright.

Both resolve, then apply the accessor in memory. That is the correct reading of the mode: it
governs the values passed *between tiles*, not how the program's inputs arrive. Resolving at the
`select!` also preserves laziness — the same point external inputs materialize today, which
matters for mmap'd regions that must not be pulled in whole.

Implementation note: this is why both base forms implement an `InlineSelectSource` trait
mirroring `SelectSource`. Without it `select!` would stop compiling for `storage!` bases the
moment the second arm existed, regardless of which arm ever ran.

### 6. No trace, therefore no trace commitment

**In unauthenticated mode the runtime installs no trace publisher, regardless of
`RASTER_TRACE_PATH`.**

`publish_trace_event` (`crates/raster-runtime/src/tracing.rs:144`) is already a no-op when
`GLOBAL_PUBLISHER` is unset, so this is a condition added to `init()` at `:77`, not new plumbing.

That single decision makes the interlock structural rather than procedural. Since `--commit`
operates host-side on the trace file (`run.rs:258`), an unauthenticated run produces **no
artifact for it to operate on**. There is no code path that constructs a trace commitment
without authenticated bindings, because there is nothing to construct one from. `--audit` fails
the same way.

Consequence to accept: `cargo raster run --no-auth` prints no step listing — the `else` branch
at `run.rs:290` has nothing to iterate. It is replaced with an explicit
`no trace recorded (--no-auth)` line so this reads as intent rather than breakage.

Read in the other direction, this is also what makes §1's default coherent: **a trace implies
authentication.** `init_with(publisher)` exists to install a trace publisher, so a caller
reaching for it has already asked for the thing that requires authenticated bindings — which is
why leaving it on the `Authenticated` default is the only self-consistent answer, not merely a
convenient one.

### 6.2 Profiling is refused, not warned

`RASTER_PROFILE_*` is rejected in unauthenticated mode: the run fails with a message rather than
producing a profile.

A profile's whole content is where time went, and this mode deletes an entire cost centre —
`record_tile_output_store_profile`, `__raster_storage_input_resolve_ns` and the output-coordinate
publish timings all measure work that no longer happens. An unauthenticated profile is therefore
not an optimistic measurement of the same program; it is an accurate measurement of a different
one, and its numbers cannot be compared against, or acted on for, a run that will actually be
committed. Emitting it behind a warning invites exactly that comparison. Refusing costs nothing:
the fix is to drop `--no-auth`.

### 6.1 The output artifact is still written

Two different things are being called a commitment, and only one of them is suppressed:

| | commits to | suppressed here? |
| --- | --- | --- |
| trace commitment (`--commit`) | **how** the bytes were computed — the step sequence and its bindings | **yes**, structurally (§6) |
| `output_manifest.json` SHA-256 | **what** the bytes are — a hash of the final value | **no** |

The output artifact's hash is honest about the bytes regardless of mode: `write_raster_files`
serializes the value it was given and hashes the result. It makes no claim about provenance, and
nothing in the artifact format asserts one.

So an unauthenticated run under `cargo raster run` still writes `output.bin` / `output.rindex` /
`output_manifest.json` when `RASTER_OUTPUT_DIR` is set. This is deliberate and load-bearing for
§10: a cheap stage that produces a real output artifact is the primitive the chain work needs.

### 7. Out of scope for v1: `Draft<S>` and recur

`Draft<S>` (`crates/raster/src/input.rs:44`) is a linear handle threading set-once writes, and
`incremental-draft-witness.md` (implemented) builds a per-step transition witness over it.
`restore_draft_from_replay_handle` (`:628`) and the recur drivers (`run_recur_list` and its eight
siblings, `:2159`–`:2634`) have no obvious unauthenticated meaning — a draft with no storage is
just `S` being mutated, which is *simpler*, but the witness machinery around it is not.

v1 rejects them: a program using `new!`, `call_recur!` or `call_recur_seq!` fails at startup with
a message naming the construct and telling the author to run authenticated. **Failing loudly at
the top is the requirement** — a half-working draft here is worse than no mode at all.

The recur drivers write storage only through the step tile's own `bind_*_call`, so extending to
them later is mostly a question of what a `Draft` *means* with no storage, not of finding more
write sites.

### 8. External inputs

Inputs load from the same `--input` / `--input-manifest` files through the same
`FileInputSourceResolver`, **skipping the commitment comparison** at
`crates/raster-runtime/src/source/file.rs:66`–`:95`.

The manifest is still required and still read, because it carries the `encoding` that says how
to decode each input; dropping it would change how inputs are declared and stop the two modes
being the same program. Only the SHA-256 equality check is skipped. Cost: a corrupted fixture
surfaces later and less clearly, as a decode failure rather than a commitment mismatch.

### 9. Serialization fidelity — where the two modes could disagree

In authenticated mode every inter-tile value round-trips through postcard: the producing tile's
return value is encoded, stored, and decoded again before the consuming tile sees it. In
unauthenticated mode the value is passed directly. **If a type's postcard round-trip is not the
identity, the two modes can produce different results.**

**For a RAS-conforming program this is very close to a non-issue**, and it is worth being precise
about why rather than leaving it as a vague hazard. The two classic sources of round-trip
infidelity are already forbidden inside tiles by **RAS-206**
(`authoring-skill-and-tooling.md:99`), which bans:

- `HashMap`/`HashSet` — *"iteration order — use `BTreeMap`/`BTreeSet` or `Vec`"*;
- floating point *"unless bit-identical cross-target behavior has been verified"*;
- pointer/address-derived values.

Those are exactly the cases where "same value, different bytes" or "same bytes, different
behaviour" arise. RAS-206 bans them for a different reason — native/guest replay divergence —
but the ban covers this too. For the types that remain (scalars, `String`, `Vec`, `BTreeMap`,
derived structs of those), **postcard's round-trip is the identity**, and the two modes agree
exactly.

What remains, in descending order of realism:

1. **`#[serde(skip)]` fields.** A skipped field is reconstructed as `Default::default()` on
   decode. Authenticated mode therefore *clears* it between tiles; unauthenticated mode carries
   the producer's value through. This is the one residual source that needs no exotic code to
   trigger, and note that it is the authenticated mode whose behaviour is surprising.
2. **Asymmetric custom `Serialize`/`Deserialize`.** A hand-written impl that normalizes, clamps
   or canonicalizes on decode. Rare, but nothing forbids it.
3. **Shared ownership.** `Rc`/`Arc` shared between two fields encodes as two copies and decodes
   as two independent allocations, so `Rc::ptr_eq` and `strong_count` differ. Only observable by
   a program that inspects identity rather than value.
4. **Allocation-shape observables.** `Vec::capacity` after a round-trip need not match.

**The gap this exposes is in the rules, not in this proposal.** RAS-203
(`authoring-skill-and-tooling.md:90`) requires tile inputs and outputs to be *"postcard-serializable
serde types"* — it does not require the round-trip to be the identity. That requirement is
already relied on everywhere (a fraud proof replays a tile from its encoded input and compares
output bytes; if decode were lossy, replay would be meaningless), it has simply never been
written down. This proposal asks for it to be:

> **RAS-203a** A tile input/output type's postcard round-trip MUST be the identity. In
> particular, no field affecting program output may be `#[serde(skip)]`, and custom
> `Deserialize` impls MUST NOT normalize.

Under RAS-203a the two modes agree by construction, and any disagreement is a rule violation
with a name — which is a better outcome than a tool that measures disagreement after the fact.

RAS-203a is part of this proposal and lands as its **last** step (§12 step 11), after the mode
it justifies exists. Sequencing it that way keeps the rule from being introduced as an abstract
constraint on authors before there is anything that depends on it.

**No `--check-both` in v1.** A mode that runs both ways and diffs was considered and dropped: it
would be tooling built to detect violations of a rule that had not been written yet. Write the
rule first. If RAS-203a later needs enforcement, it belongs with the other unenforced authoring
rules in `authoring-skill-and-tooling.md` §3, not here.

### 10. Chains are a separate proposal

`cargo raster chain run` today commits every stage, so it sets `RASTER_AUTH=1` and is otherwise
untouched by this proposal.

The direction it unlocks — recorded here so the dependency is visible, and deliberately **not**
designed here — is to stop paying for commitment on stages nobody is disputing: run the chain's
stages cheaply to produce their output artifacts, and enter commitment mode only for the stage
whose output is contested. That turns per-stage commitment from a fixed cost of running a chain
into a cost paid on demand.

Two things this proposal deliberately provides for it, and nothing more:

- a stage execution that is cheap (§3–§5), and
- an output artifact that a cheap stage still produces, so it can feed the next stage (§6.1).

Everything else that idea needs — how a contested stage is identified, what a chain commitment
means when its stages were run at different postures, whether a mixed chain is a coherent object
at all — is chain-level policy and belongs with `program-chain.md` / `chain-fraud-proof.md`.

### 11. Documentation requirements

All landed:

- **`01-runner-modes.md` §"Mode: native"** gains an "Authentication posture" subsection (§2):
  what the mode changes, the full resolution order, why `cargo test` is authenticated, and the
  `init()` caveat below.
- **`.claude/skills/raster/SKILL.md` §9** gains **rung 0** (`cargo run`) plus a preamble stating
  it is not authoritative, with rung 3 relabelled the first authoritative rung. Its failure table
  gains two rows: a rung-0/rung-3 value disagreement (→ RAS-203a) and the draft/recur refusal.
- **RAS-203a** is in `authoring-skill-and-tooling.md`, both in the rule list and in the
  `cargo raster check` enforcement table — detectable for the `#[serde(skip)]` case, which is the
  one that needs no exotic code; a hand-written normalizing `Deserialize` stays undetectable.
- **The `init()` caveat** — calling `raster::init()` outside a `#[sequence] main` opts that binary
  into unauthenticated execution — is in the spec and on `init`'s own rustdoc, which is where
  someone about to do it will actually be looking.
- Prose must keep `AuthMode::Unauthenticated` distinct from the pre-existing `Inline` vocabulary
  (`AuthRef::Inline`, `FnInputValue::Inline`, `InputSource::Inline`), which means something else:
  "an argument with no upstream binding".

## Implementation plan

1. **`AuthMode` + resolution** — `raster-runtime`: enum, `OnceLock` cache, `RASTER_AUTH` parsing,
   and the `init()`-lowers-the-default rule (§1). Small, self-contained.
2. **Trace suppression** — one condition in `tracing::init()` (`:77`). Do this *second*, before
   any storage change, so the interlock exists before the thing it guards.
3. **Profiling refusal** — reject `RASTER_PROFILE_*` in unauthenticated mode (§6.2).
4. **Write side** — `bind_infallible_call` / `bind_fallible_call`.
5. **Read side** — `into_auth_value_with_bindings`.
6. **`select!` dual lowering** — the bulk of the work. Order: `BytesPage` first (§5.2), then
   scalar/struct paths, then `List`/`Block`, then dynamic indices.
7. **Reject `Draft`/recur at startup** with a message naming the construct.
8. **External input check skip** — `source/file.rs`.
9. **CLI** — `--no-auth` on `cargo raster run`, `conflicts_with = ["commit", "audit"]` as defence
   in depth even though §6 makes it moot; `RASTER_AUTH` in the child env; mode printed on every
   run; the `run.rs:290` else-branch message.
10. **Docs** — `01-runner-modes.md` §"Mode: native"; check-ladder rung 0; the `init()` caveat
    from §1.
11. **RAS-203a** (§9) into the authoring rules — **last**, once the mode it justifies exists.

Steps 1–5 are small. Step 6 is the proposal.

**No test changes are required at any step.** The §1 rule makes `cargo test` authenticated
because no test reaches `init()`; this was verified against all nine storage-exercising files
under `crates/raster/tests/`. Any test that *wants* the unauthenticated path must set
`RASTER_AUTH=0` explicitly, which is the right shape for something that switches verification
off inside a verification test suite.

## Open questions

None outstanding. Resolved during review:

- **`cargo test` is always authenticated** — a requirement, not a default (§1). Raster's tests
  exist to check that authentication is correct.
- **`RASTER_AUTH` is the spelling**, despite the other runtime env vars naming paths.
- **RAS-203a belongs in this proposal**, as the last implementation step (§9, §12 step 11).
- **Profiling is refused in unauthenticated mode**, not warned (§6.2).
- **The output artifact is still written** (§6.1) — its hash commits to bytes, not provenance,
  and a cheap stage that still produces one is what §10 needs.
- **No `--check-both`** (§9) — write the rule before building a tool to measure violations of it.
