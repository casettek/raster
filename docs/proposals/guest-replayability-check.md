# Proposal: `guest-replayability-check` — decide RISC0 replayability without proving

Status: proposed 2026-08-16

Related:
- [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) — **owns the rule set.**
  RAS-101/206/208 state the replayability rules; §3 Tier 2 gives `cargo raster check` one
  heuristic row for them; §6 Non-goals ends with "true enforcement is the zkVM replay itself".
  This proposal makes that replay cheap enough to be a check rather than a release gate, and
  adds three rules (RAS-209, RAS-210, RAS-602) to that document's set.
- `docs/specs/core/3-execute/04-zkvm-preview-execution.md` — specifies `ExecutionMode::Estimate`,
  the executor path this proposal turns into the check.
- `docs/specs/core/2-compile/03-tile-artifact-generation-elfs.md` — the guest crate template and
  toolchain discovery a cross-compile probe reuses.
- [`program-identity.md`](./program-identity.md) — `recipe_fingerprint` is the cache key that
  §4 G-2 asks to widen.

## Problem

A Raster tile that compiles and runs natively is not thereby executable as a RISC0 guest. Three
distinct things can be wrong, and today all three surface late:

| Failure class | Example | Surfaces today at | Cost to find out |
| --- | --- | --- | --- |
| **Doesn't build for `riscv32im`** | `std::collections::HashMap` in the tile lib; a dep without a `no_std` posture | `compile_tile`, i.e. inside `cargo raster run --audit` | full `cargo build --release` with `lto = true`, **per tile** |
| **Builds, aborts in the guest** | index out of bounds; `Vec` past the guest address space; `unwrap` on a value that only exists on the host | first proving attempt | executor + prover, per step |
| **Builds, runs, produces different bytes** | `usize` truncation; `HashMap` iteration order; a `libm` transcendental | audit divergence, or never | full commit/audit round-trip |

The check ladder (`.claude/skills/raster/SKILL.md` §9) has six rungs. Exactly one of them —
rung 4, `cargo raster run --audit` — ever touches the zkVM, and it does so with
`ExecutionMode::prove_and_verify()` (`raster-cli/src/commands/run.rs:757`). So the author's
options are: an AST deny-list that `authoring-skill-and-tooling` §6 correctly refuses to call a
proof, or the most expensive operation the toolchain can perform.

Worse, rung 1 gives **false confidence**. `cargo check -p <lib> --no-default-features`
type-checks for the *host*, where `usize` is 64 bits, where `std` is still linkable by any
transitive dep that didn't gate it behind that feature, and where the target triple never
enters resolution. It answers a different question than the one the author thinks they asked.

## The fact this rests on

The RISC0 executor is not a simulator that approximates the prover. It **is** the prover's
execution stage: `default_prover().prove()` executes the ELF to produce segments and then
proves those segments. `default_executor().execute()` stops after the first half.

Therefore, for a given input:

> If `execute()` returns a session, `prove()` on the same ELF and input will produce a receipt,
> subject only to *host* resources (RAM, GPU) — not to anything about the program.

Segment po2 limits are enforced during execution, so a program too large to segment fails in
the executor too. This is what turns the question from an estimate into a **decision**, at
executor cost.

The honest limit: it is a decision **for the inputs actually run**. A tile that survives a
64-row fixture and blows the address space at 64k rows is not caught by replaying the 64-row
fixture. That is what §3 Tier 3's cycle headroom is for, and it is a budget argument, not a
proof.

## 1. Design: `cargo raster zkcheck`, four tiers

Cheapest first. Each tier is independently useful and independently landable; the ordering in
§5 is by value-per-unit-effort, which is *not* the same order.

```bash
cargo raster zkcheck                     # tiers 0–1, no fixture needed
cargo raster zkcheck --replay commit.bin # tiers 0–3, full verdict
cargo raster zkcheck --tier 2 --replay commit.bin --format json
```

Exit non-zero on any `FAIL`; `--deny warn` promotes budget warnings.

### Tier 0 — static, no toolchain, milliseconds

An AST pass over tile bodies and their module graph. **This tier is not new work owned here** —
it is `authoring-skill-and-tooling` §3 Tier 2's RAS-206 row, in
`raster-compiler/src/validate.rs`, reached from `cargo raster check`. `zkcheck` invokes it and
adds the target-width rows that document does not have (§4 G-1, G-2).

Catches: deny-listed idioms, `usize` in ABI types, profile divergence.
Misses: everything semantic. It is a lint, and this proposal does not upgrade that claim.

### Tier 1 — cross-compile probe, seconds

One synthesized probe crate, one `cargo check`, all tiles:

```
target/raster/zkcheck/probe/
  Cargo.toml   # [workspace] (empty), risc0-zkvm, raster { default-features = false },
               # <user lib> { default-features = false }
  src/lib.rs   # #![no_std] extern crate alloc;
               # use <user_lib>::{__raster_tile_replay_entry_a, …};
               # static PROBES: &[fn(&[u8]) -> raster::core::Result<alloc::vec::Vec<u8>>] = &[…];
```

then, with the toolchain from `GuestBuilder::find_risc0_cargo`:

```bash
cargo check --target riscv32im-risc0-zkvm-elf --message-format=json
```

Naming every replay wrapper in a `static` array of fn pointers is what forces the wrappers —
and therefore every tile signature's postcard bounds — to be resolved and type-checked, rather
than dead-code-eliminated before the type checker cares.

Catches: `std` leakage, deps without a `riscv32` posture, 64-bit-only types, postcard/serde
bound failures in generated wrappers. Definitively, and for **every** tile in one invocation.

Misses, and this is a real limit: `cargo check` does no codegen, so it cannot catch link-stage
failures — the duplicate `panic_impl` E0152 that `guest_builder.rs` documents in its Cargo.toml
comment being the known example. Tier 1 is necessary, not sufficient, for "it builds". The full
ELF build in `compile_tile` stays authoritative.

Cost: one `cargo check`, no codegen, no LTO, no per-tile fan-out — against N full
`cargo build --release -C lto` today.

### Tier 2 — executor dry-replay, the completeness check

This is the tier that answers the question, and it is nearly free to build.

```bash
cargo raster run --input input.json --input-manifest input_manifest.json \
  --commit commit.bin --fraud-proof-window-size 32
cargo raster zkcheck --replay commit.bin
```

For each tile step in the trace: compile the guest (cached), execute it in the RISC0 executor
with the recorded input witness, and compare.

Mechanically it is `run.rs`'s audit path — `Replayer::replay(tile_id, input, mode)` at
`run.rs:944` — with `ExecutionMode::Estimate` in place of `prove_and_verify()`, and with the
transition-guest and receipt machinery skipped. `Replayer::replay` already takes `mode` as a
parameter; `Risc0Backend::execute_tile`'s `Estimate` arm already decodes `TileReplayJournal`
and returns cycles.

Two comparisons per step:

1. `TileReplayJournal.output_bytes` **==** the output recorded in the trace. A mismatch is
   nondeterminism or a cross-target semantic difference (§4) — the class that today only shows
   up as an audit divergence, if the fixture happens to hit it.
2. `TileReplayJournal.input_commitment` **==** `sha256(replay_input)`. Already the contract;
   checking it here localizes witness-assembly bugs to the tile rather than to the guest.

**Replay the whole trace, not a window.** The fraud-proof window exists because proving is the
budget. Remove the prover and the reason for the window goes with it; "is my program
replayable" is a question about every step, and at executor cost every step is affordable.

Catches: guest aborts (panic, OOB, allocation past the guest address space), output divergence,
and — because a session carries cycle counts — the input to Tier 3.
Misses: input-dependent failures outside the fixture's coverage.

### Tier 3 — cycle and cost budget

From the same sessions, per tile: max cycles observed, `calculate_proof_cycles(cycles)` (the
po2 padding in `raster-backend/src/backend.rs`), and a verdict against a budget:

```toml
# Raster.toml
[zkcheck]
warn_tile_cycles = 8_000_000
max_tile_cycles  = 33_554_432
```

Report:

```
tile           steps   max cycles   proof cycles   status
parse_header       1      412_003        524_288   ok
score_row        512    1_204_889      2_097_152   ok
render             1   18_442_110     33_554_432   WARN  over warn_tile_cycles
summarize          1            —              —   FAIL  guest abort:
                                                          index out of bounds: len 0, index 3
```

This is RAS-208 ("tiles MUST stay small enough to replay") given a number instead of an
adjective, and it is the tier that reads as *estimation* — proof cost, and headroom before the
next po2 doubling, without a proof.

## 2. What each tier costs, relative to today

| | today | with `zkcheck` |
| --- | --- | --- |
| "does my tile build as a guest?" | N × `cargo build --release -C lto` inside an audit run | 1 × `cargo check --target riscv32im` |
| "does my tile run as a guest?" | prove every window step | execute every step, no STARK |
| "does it produce the same bytes?" | commit/audit round-trip | same executor pass, byte compare |
| "what will it cost to prove?" | prove it | cycle table from the same pass |

## 3. New rules for the `authoring-skill-and-tooling` set

These extend §1 of that proposal rather than living here; `zkcheck` is their enforcement.

- **RAS-209** No `usize`/`isize` in any type crossing the tile ABI (parameters, return types,
  and any field reachable within them). `usize` is 64-bit natively and 32-bit in the guest.
  Use `u32`/`u64` explicitly. *[Tier 0 — signature and reachable-type scan]*
- **RAS-210** The generated guest crate declares its own `[profile.release]` and an empty
  `[workspace]`, so the user's release profile does not reach guest builds. A project whose
  `[profile.release]` sets `overflow-checks` or `debug-assertions` diverges from its own guests.
  Either keep those unset, or accept that `zkcheck` mirrors them into the guest template.
  *[Tier 0 — Cargo.toml comparison]*
- **RAS-602** `cargo raster zkcheck` is rung 3.5 of the check ladder — after the native run,
  before the commit/audit round-trip. A failing `zkcheck` means the audit cannot pass, found
  without proving.

## 4. The divergence hazard list

Not currently recorded in any document in this repo. Each row names the tier that catches it.

- **G-1 — `usize` is 32 bits in the guest.** `u64 as usize` truncates in the guest and not
  natively; `usize` arithmetic overflows at a different point. Postcard varint-encodes `usize`,
  so the wire format survives below 2³², but the *value* need not. → RAS-209. **Tier 0**
  (types), **Tier 2** (values, if the fixture reaches them).
- **G-2 — profile divergence.** `generate_guest_cargo_toml` writes
  `[profile.release] opt-level = 3, lto = true` plus an empty `[workspace]`; the native run is
  `cargo build --release` in the user's workspace (`run.rs:77`) under the user's profile. Any
  semantic key the user sets — `overflow-checks`, `debug-assertions` — applies natively and not
  in the guest: a native panic becomes a silent guest wrap. Fix is small: mirror those two keys
  into the template and fold them into `GuestBuilder::recipe_fingerprint` (a
  `GUEST_BUILD_ABI` bump — it changes image ids). → RAS-210. **Tier 0**.
- **G-3 — `HashMap`/`HashSet` iteration order.** Already RAS-206. The guest links `std` (see the
  Cargo.toml comment in `guest_builder.rs`), so `RandomState` exists there too and need not
  agree with the host's. **Tier 0** (deny-list), **Tier 2** (divergence).
- **G-4 — floating point.** `riscv32im` has no F/D extension, so floats are soft-float via
  compiler-rt. Basic IEEE-754 ops agree; `libm` transcendentals (`sin`, `exp`, `powf`) route
  through a different implementation than the host's and need not be bit-identical.
  **Tier 0** flags the calls, **Tier 2** decides whether they actually diverged.
- **G-5 — guest address space.** A tile that allocates freely on a host with 64 GB fails in a
  bounded guest address space. This is RAS-208's only real teeth. **Tier 2 only** — no static
  analysis sees it.
- **G-6 — cycle blowup.** Not a failure; a bill. **Tier 3**.
- **G-7 — panics are unrecoverable.** `generate_guest_main` calls
  `.expect("Tile runtime failure")`, and the spec records that there is no structured error
  channel from guest to host. Any tile panic is an abort with a message that has to be scraped
  from the executor error. **Tier 2**, and the reason Tier 2's reporting should surface the
  executor's message verbatim rather than wrapping it.
- **G-8 — `std` leakage.** The tile lib must compile for `riscv32im` under
  `default-features = false`. **Tier 1** definitively; rung 1 of the current ladder does *not*
  cover this, per §Problem.

## 5. Implementation

| Tier | Files | Shape |
| --- | --- | --- |
| 2 | `raster-cli/src/main.rs` (subcommand), new `raster-cli/src/commands/zkcheck.rs`, factored trace-decode from `commands/run.rs` | `ExecutionMode::Estimate` through the existing `Replayer`; drop window restriction; two byte comparisons; report |
| 3 | `raster-cli/src/commands/zkcheck.rs`, `raster-compiler/src/project.rs` (`[zkcheck]` table) | cycles are already on `TileExecutionResult`; `calculate_proof_cycles` already exists |
| 1 | new `raster-backend-risc0/src/probe.rs`, reusing `GuestBuilder::find_risc0_cargo` | generate probe crate, run `cargo check --message-format=json`, map diagnostics |
| 0 | `raster-compiler/src/validate.rs` (owned by `authoring-skill-and-tooling` §3 Tier 2) | two new rows: RAS-209, RAS-210 |

Also: `.claude/skills/raster/SKILL.md` §9 gains rung 3.5, and its failure→rule table gains rows
for guest abort and executor output mismatch.

**Recommended order: 2, 3, 1, 0.** Tier 2 is by far the best ratio — it is a mode flag through a
path that already exists, and it is the only tier that *decides* the question. Tier 3 is
near-free once 2 lands (the numbers are already in hand). Tier 1 is the largest new surface and
the one that most improves the inner loop. Tier 0 should land with `cargo raster check`, not
ahead of it.

## 6. Non-goals

- **No new proving, trace-format, or commitment work.** `zkcheck` reads a `--commit` artifact
  and produces a report; it emits nothing that any verifier consumes.
- **No purity proof.** §6 of `authoring-skill-and-tooling` holds. Tier 0 is a lint; the upgrade
  offered here is that Tier 2 makes the real enforcement cheap, not that the lint got stronger.
- **No replacement for the audit round-trip.** A green `zkcheck` says every tile ran in the zkVM
  and produced the recorded bytes. It says nothing about trace honesty, CFS conformance, or
  fraud-proof soundness — the transition guest owns those, and rung 4 stays in the ladder.
- **No input-space coverage claim.** `zkcheck` is exactly as complete as the fixture it replays.

## 7. Uncertainties

1. **Should `zkcheck` run its own native pass?** Requiring `--commit` first is one extra command
   and reuses a real artifact. Folding it in (`zkcheck --input … --input-manifest …` running
   native-then-replay) is friendlier but duplicates `run.rs`'s orchestration. Leaning toward
   requiring the artifact and adding the fold-in later if the friction shows.
2. **Whole-trace replay on large traces.** "Every step" is right in principle; a 100k-step trace
   at executor speed may still want `--sample N` or `--steps a..b`. Deferred until a trace big
   enough to hurt exists.
3. **G-2's fix changes image ids.** Mirroring `overflow-checks` into the guest template is
   correct but is a `GUEST_BUILD_ABI` bump touching `program_commitment` consumers. It may be
   cleaner to ship Tier 0's *detection* of G-2 first and make the template change a separate,
   deliberate identity break.
4. **Tier 1's probe crate and feature unification.** The probe depends on the user lib with
   `default-features = false`, but if the probe's own dep graph pulls a shared crate with `std`,
   feature unification inside the probe's workspace could mask a leak that the real guest hits.
   Empty `[workspace]` plus resolver v2 mirrors the guest template exactly, which is the
   mitigation; it wants a test that a deliberately `std`-leaking tile is caught.
