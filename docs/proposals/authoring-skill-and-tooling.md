# Proposal: Raster authoring skill + `cargo raster check` — guided and machine-checked authoring of verifiable programs

Status: proposed
Related: [`program-start.md`](./program-start.md), [`program-end.md`](./program-end.md),
[`program-chain.md`](./program-chain.md), [`program-identity.md`](./program-identity.md),
`docs/tile-authoring.md`, `docs/specs/core/1-define/*`

## Problem

Raster programs are only verifiable if the author follows a set of hard rules that the
type system alone cannot express:

- **Computation lives in tiles, orchestration lives in sequences.** A `format!` in a
  sequence body is legal Rust, compiles, and runs natively — but it is invisible to the
  CFS, unproven by any tile replay, and silently breaks the "everything that computes
  is re-executable in the zkVM" invariant.
- **Tiles must be re-executable as RISC0 guests.** They must be `no_std + alloc`,
  postcard-serializable at the ABI boundary, deterministic (bit-identical output
  between the native run and guest replay), and small enough to replay within a proof
  window. None of this is enforced by `cargo build` for a `std` host target.
- **Data must flow through authenticated channels.** Entry arguments come from
  committed external inputs; sub-values are accessed via `select!` (selection
  commitments); large data is walked in blocks via `call_recur!`/`call_recur_seq!`
  whose step-function shapes are rigidly constrained; program output must be a
  storage-backed value (`ProgramEnd`).

Today these rules live in four places with different levels of authority and coverage:
`docs/tile-authoring.md` (partially stale), `docs/specs/core/1-define/*` (predates
`call!`/`select!`/recur-drafts), macro panics/`compile_error!`s (authoritative but only
for what they cover), and the heads of the people who wrote the recent proposals.

Two consequences:

1. **An AI agent (or a new contributor) writing a Raster program has no single strict
   document to follow.** It will pattern-match on general Rust habits — computed
   expressions in call arguments, `HashMap` in tiles, destructured `let` bindings,
   whole-collection tile inputs — and produce programs that compile and run natively
   but cannot be committed, audited, or fraud-proven.
2. **Feedback arrives too late.** Some violations surface as macro panics (good), some
   as runtime storage errors, some only when `--commit`/`--audit` diverges, and some
   (determinism) never surface until a guest replay disagrees with the native trace.

## Goal

Two deliverables that share one normative rule set:

1. **A strict authoring skill** (`.claude/skills/raster-authoring/`) that guides an AI
   agent step-by-step through writing a verifiable Raster program — tiles, sequences,
   recur-tiles/recur-sequences over blocks of data, program boundary, chaining — with
   every rule stated as a MUST/MUST NOT and paired with the check that verifies it.
2. **A `cargo raster check` static validator** plus an editor/rust-analyzer integration
   path, so the same rules the skill states in prose are enforced mechanically while
   the code is being written — for agents *and* humans.

The rule set below (§1) is the single source of truth both deliverables consume. Each
rule carries a stable ID (`RAS-xxx`) so skill text, validator diagnostics, and CI logs
all reference the same thing.

## 1. The authoring rule set (normative)

Rules are grouped by layer. Each rule notes its current enforcement:
**[macro]** = compile-time via `#[tile]`/`#[sequence]` expansion (rust-analyzer shows
these live), **[runtime]** = fails during `cargo raster run`, **[audit]** = only
detected by commit/audit divergence, **[none]** = currently unenforced — skill +
`cargo raster check` are the only guards.

### A. Program shape (RAS-1xx)

- **RAS-101** A program is one crate with `#![no_std]` tile library (`src/lib.rs`,
  `extern crate alloc`) and a `std` binary (`src/main.rs`) holding the sequences and
  the `#[sequence] fn main` entry point. Tiles MUST live in code that compiles for
  `riscv32` (the RISC0 guest builds the user crate with `default-features = false`).
  [runtime — guest build failure, very late]
- **RAS-102** The entry point is `#[sequence] fn main`. Its parameters are *entry
  arguments*: each MUST have a matching committed external input (name-keyed in
  `input.json` / `input_manifest.json`, postcard- or raster-encoded files). [runtime]
- **RAS-103** Tile and sequence IDs are the bare Rust function names and MUST be
  unique across the program. [none — discovery does not reject duplicates]
- **RAS-104** Shared input types used with `select!` MUST derive
  `Selectable` (plus `Serialize`/`Deserialize`) and live in the `no_std` library.
  [macro/type errors, but with poor locality]

### B. Tiles — all computation (RAS-2xx)

- **RAS-201** Every computation that affects program output MUST happen inside a
  `#[tile]` function. Sequences MUST NOT compute (see RAS-3xx). [none]
- **RAS-202** A tile is a non-generic free function; parameters are simple
  identifiers (`x: T`, no destructuring, no `self`). [macro — partially; destructured
  params can panic the macro without a span]
- **RAS-203** Tile inputs/outputs MUST be postcard-serializable serde types. The ABI
  is: 0 args → `postcard(())`, 1 arg → `postcard(arg)`, N args → `postcard(tuple)`;
  output is `postcard(return value)`. [type errors in generated wrapper]
- **RAS-204** Attribute syntax is key/value only: `#[tile(kind = iter)]` (default) or
  `#[tile(kind = recur)]`. Positional `#[tile(recur)]` is silently ignored — never
  write it. Optional: `description`, `estimated_cycles`, `max_memory`. Unknown keys
  are silently ignored today. [macro for bad `kind` value only]
- **RAS-205** Fallible tiles return the prelude `Result<T>` (Raster's terminal
  execution result). Call sites propagate with `?` on the `call!` result. [type errors]
- **RAS-206** Tiles MUST be deterministic and self-contained: no I/O, filesystem,
  network, clock, randomness, threads, or environment access. `no_std` removes most of
  the temptation, but the rule extends to anything that makes native execution and
  guest replay diverge bit-for-bit: no `HashMap`/`HashSet` (iteration order — use
  `BTreeMap`/`BTreeSet` or `Vec`), no floating point unless bit-identical cross-target
  behavior has been verified, no pointer/address-derived values, no
  `#[cfg(target_...)]`-dependent logic in tile bodies. [none — the critical gap]
- **RAS-207** Logging in tiles/sequences uses `raster::println!` only (the CLI
  captures it); `std::println!` is unavailable in tiles and bypasses the CLI in
  sequences. [compile error in tiles, none in sequences]
- **RAS-208** Tiles MUST stay small enough to replay in the zkVM: never take a whole
  large collection as one argument to "keep data together". Walk collections in
  blocks with `select!` slices and `call_recur!`/`call_recur_seq!` (§E) so each replay
  unit stays bounded. Use `estimated_cycles` to document known-heavy tiles. [none]

### C. Sequences — orchestration only (RAS-3xx)

- **RAS-301** A sequence body consists ONLY of: `call!`, `call_seq!`, `call_recur!`,
  `call_recur_seq!`, `select!`, `new!`, `storage!`, `finalize(...)`, simple `let`
  bindings of those results, `?`/`.expect` on fallible calls, `.clone()` of bindings,
  `raster::println!`, and the final return expression. Anything else — arithmetic,
  string formatting, conditionals on values, loops, method calls that compute — is
  computation and MUST move into a tile. [none — the highest-value check]
- **RAS-302** Every tile invocation uses `call!(tile_name, args...)`; every
  sub-sequence invocation uses `call_seq!(seq_name, args...)`. Bare calls are not
  extracted into the CFS and MUST NOT be used. [none — bare calls silently produce a
  wrong CFS]
- **RAS-303** No nested calls. `call!(f, call!(g, x))` and `exclaim(greet(name))` are
  both forbidden; decompose into sequential `let` bindings so every step boundary and
  dataflow edge is explicit. [none]
- **RAS-304** `let` bindings of call results MUST be simple identifiers (`mut` is
  fine). Destructuring (`let (a, b) = ...`) breaks binding resolution — a tile that
  logically returns several values returns one struct/tuple, and consumers `select!`
  into it. [none — silently degrades bindings to external inputs]
- **RAS-305** Call arguments MUST be: a sequence parameter, a prior `let` binding
  (optionally `.clone()`), a `select!`/`new!`/`storage!` expression, or a literal.
  Computed expressions (`x + 1`, `format!(...)`, `vec![...]`) as arguments are
  forbidden — they are unauthenticated inline inputs. [none]
- **RAS-306** Control flow in sequence bodies (if/match/for/while on runtime values)
  is not represented in the CFS and MUST NOT be used. The only sanctioned iteration
  is `call_recur!`/`call_recur_seq!`; the only sanctioned early exit is
  `RecurControl::Break` inside a recur step and `?` on fallible calls. [none]
- **RAS-307** Sub-value access goes through `select!(Type, binding.path)` — field
  access (`.name`), indexing (`[0]`), and contiguous slices (`[0..2]`, one selection
  commitment for the whole slice) over `Selectable` types. Direct field access on a
  binding outside `select!` is computation (RAS-301). [none]
- **RAS-308** Multi-tile output construction uses the draft protocol: `new!(T)` →
  thread `Draft<T>` through tiles that mutate via set-once accessors
  (`.field().set(...)`, `.list().push(...)`) → `finalize(draft)`. Draft handles are
  linear: no clone, no reuse after move. [macro — UI-tested]

### D. Recur tiles and recur sequences — blocks of data (RAS-4xx)

- **RAS-401** Iterating a list is done with `call_recur!` (tile step) or
  `call_recur_seq!` (sequence step), never with a Rust loop. The input MUST be a
  selectable storage-backed list (a `select!`ed `Vec<T>`, a `storage!` reference, or a
  prior tile output) — `call_recur!` rejects non-storage sources at runtime. [runtime]
- **RAS-402** Recur tile signature shape is fixed and validated:
  `fn step(input: RecurInput<T>, [state: RecurState<S>,] [output: RecurOutput<O>,]
  extra_args...)`. `input` is first; at least one of `state`/`output` is present;
  `state` comes before `output`; extras follow. [macro — UI-tested]
- **RAS-403** The return type MUST match the mode: `RecurState<S>` (state-only
  reduce), `RecurOutput<O>` (output-only build), or
  `RecurControl<(RecurState<S>, RecurOutput<O>)>` with `Continue`/`Break` for
  early-terminating state+output. [macro — UI-tested]
- **RAS-404** `call_recur!` named-argument form is fixed:
  `call_recur!(tile = step_fn, input = <list source>, [chunk = <int literal>,]
  [state = <initial S expr>,] [output = new!(O),] args = (extras,))`. `state` and/or
  `output` must match the step function's mode; `chunk = N` must be an integer
  literal (it is pinned in the CFS) and switches the step's element type from `T` to
  `Vec<T>`. [macro]
- **RAS-405** Inside the step: `input.value()`/`.into_value()` for the element,
  `input.index()` for position, `input.is_first()` for init-once logic (e.g. setting
  a draft title on the first iteration). `RecurOutput` uses draft set-once semantics.
  Empty inputs skip the step entirely and only finalize if the untouched output
  schema materializes without set-once writes — design outputs accordingly. [runtime]
- **RAS-406** Recur *sequences* (`#[sequence(kind = recur)]`) orchestrate multiple
  tiles per element: signature uses `RecurSequenceInput<T>` /
  `[RecurSequenceState<S>]` / `[RecurSequenceOutput<O>]` mirroring RAS-402; the body
  passes `input`/`state`/`output` handles *opaquely* to tiles via `call!` — it MUST
  NOT read `input.value()` itself (that would be sequence-level computation) and MUST
  NOT return `RecurControl` (early exit belongs to tile steps). `main` cannot be
  `kind = recur`. [macro — UI-tested]

### E. Program boundary and chaining (RAS-5xx)

- **RAS-501** `main`'s return value is the program's authorized output (`ProgramEnd`).
  It MUST be `()` or a storage-backed value: the result of a `call!`/`call_seq!`/
  `call_recur!`/`select!`. Returning an inline literal or locally computed value as
  the program output is an error. [runtime]
- **RAS-502** The exported output artifact (`output.bin` + manifest) is
  format-compatible with external inputs: in a chain, stage N's output is stage N+1's
  committed input. Design chained programs so the next stage's `main` entry argument
  type matches the previous stage's output type. [audit — `chain audit` link checks]
- **RAS-503** Program identity is pinned by `Raster.toml`/`Raster.lock`
  (`program.bin` is a regenerable build cache). After changing tiles or sequences,
  run `cargo raster program --verify` (and re-lock intentionally) so replay/fraud
  pipelines see the change as deliberate. [CLI check exists, not automatic]

### F. Verification workflow (RAS-6xx)

- **RAS-601** A Raster program change is not "done" at `cargo build`. The full check
  ladder is: `cargo check` (both `std` and `no_std` feature posture) →
  `cargo raster cfs` (inspect that every intended step appears with the right
  bindings — no silent `External` fallbacks where a prior-output binding was meant) →
  `cargo raster run --input ... --input-manifest ...` → commit/audit round-trip
  (`--commit` + fraud-proof window, then `--audit`) → `cargo raster program --verify`
  → for chains, `cargo raster chain run` + `chain audit --execution`.

## 2. Deliverable 1: the `raster-authoring` skill

### Location and shape

```text
.claude/skills/raster-authoring/
  SKILL.md              # rules-first guide, the RAS index, the workflow ladder
  references/
    tiles.md            # RAS-2xx expanded: ABI, determinism list, no_std checklist
    sequences.md        # RAS-3xx expanded: the allowed-statement grammar + examples
    recur.md            # RAS-4xx expanded: all 3 modes × tile/sequence, chunking
    data-and-io.md      # select! DSL, Selectable, drafts, input.json/manifest, output
    checklists.md       # pre-commit checklist, cfs-reading guide, failure→rule map
```

Shipping it in-repo (not in `~/.claude`) is deliberate: the skill must evolve in the
same PRs that change the macros/runtime, and the UI tests under
`crates/raster/tests/ui/` double as the skill's regression suite — every example in
the skill should be a compiling doctest or mirror a UI test.

### Design principles for the skill text

1. **Rules first, rationale second.** Each section opens with the MUST/MUST NOT list
   (with RAS IDs), then a *minimal* correct example, then the most common wrong
   version labeled with the failure it causes ("compiles, runs natively, breaks
   audit" is the key phrase that teaches an agent why it can't trust `cargo build`).
2. **The two-sentence mental model up front:**
   *Tiles compute; sequences route. If a line in a sequence does anything other than
   name a step, select data, or bind a result, it belongs in a tile.*
3. **Decision tree for data over blocks** (the part agents get wrong most):
   - one value / sub-value → `select!`
   - contiguous slice as one input → `select!` with `[a..b]`
   - fold list → single value → `call_recur!` + `RecurState`
   - map list → built object → `call_recur!` + `RecurOutput` (+ `chunk = N` for
     block-sized steps)
   - both, or early stop → `RecurControl<(state, output)>`
   - multiple tiles per element → `#[sequence(kind = recur)]` + `call_recur_seq!`
   - whole large collection into one tile → **never** (RAS-208)
4. **The workflow ladder (RAS-601) is mandatory**, phrased as skill steps with the
   exact commands, and "definition of done" = the audit round-trip passes, not the
   build.
5. **Strictness posture:** the skill instructs the agent to *refuse to emulate*
   missing features with plain Rust (e.g. no `if` orchestration because CFS can't see
   it — restructure into tiles returning data instead), and to surface the limitation
   to the user rather than silently produce an unverifiable program.

### SKILL.md skeleton (condensed)

```markdown
---
name: raster-authoring
description: Write/modify verifiable Raster programs (tiles, sequences, recur
  over data blocks). MUST be used for any change under a Raster project's src/
  or Raster.toml. Enforces RISC0-replayability rules; pairs every rule with the
  command that checks it.
---

# Writing verifiable Raster programs

Tiles compute; sequences route. Every violation below compiles — that is why
this skill exists. Run the check ladder (§6) before declaring any change done.

1. Program layout (RAS-1xx) ...
2. Tiles (RAS-2xx) ...            → references/tiles.md
3. Sequences (RAS-3xx) ...        → references/sequences.md
4. Data in blocks (RAS-4xx) ...   → references/recur.md
5. Boundary & chains (RAS-5xx) ...→ references/data-and-io.md
6. Check ladder (RAS-601): cargo check → cargo raster check → cargo raster cfs
   → cargo raster run → commit/audit round-trip → program --verify
```

## 3. Deliverable 2: tooling — `cargo raster check` and editor integration

The question "can we have rust-analyzer-like tools while writing tiles/sequences?"
decomposes into three tiers, cheapest first. The key observation: **rust-analyzer
expands proc-macros live**, so every validation that lives *inside* `#[tile]` /
`#[sequence]` as a span-aware error already surfaces in the editor as you type, with
no LSP work at all. Raster already exploits this (the `tests/ui/*` diagnostics); the
plan is to push much more of the rule set into that channel and cover the rest with a
CLI validator that plugs into rust-analyzer's check-on-save.

### Tier 1 — span-aware macro diagnostics (extend what works today)

Move remaining macro `panic!`s to `syn::Error::to_compile_error` with precise spans,
and add new in-macro validations where the macro already sees enough syntax:

| New in-macro check | Rule |
| --- | --- |
| Bare call to a known-tile/sequence name inside `#[sequence]` body (the macro already walks the body for `call!` rewriting) | RAS-302 |
| Nested `call!`/`call_seq!` inside another call's argument list | RAS-303 |
| Destructuring `let` of a `call!` result | RAS-304 |
| `#[tile(recur)]` positional form and unknown attribute keys → hard error instead of silent ignore | RAS-204 |
| Non-identifier tile parameters → spanned error instead of macro panic | RAS-202 |
| `if`/`match`/`for`/`while` statements in `#[sequence]` bodies → error (with an escape hatch attribute while control-flow support is designed) | RAS-306 |

Each lands with a `trybuild` UI test; the `.stderr` files become skill reference
material for "what the error looks like".

**Effort: low. Payoff: live in-editor squiggles in any rust-analyzer client, zero new
infrastructure.** This is the closest thing to "rust-analyzer support" and should come
first.

### Tier 2 — `cargo raster check`: whole-program static validation

Some rules need cross-function/project knowledge a single macro expansion can't have
(duplicate IDs, computation-statement classification against the known tile/sequence
registry, determinism heuristics, CFS-binding sanity). `raster-compiler` already
parses the whole project with `syn` (`ProjectAst`, `TileDiscovery`,
`SequenceDiscovery`, `FlowResolver`) — the validator is a new consumer of that data,
not new parsing infrastructure.

New module `raster-compiler/src/validate.rs` + CLI command:

```bash
cargo raster check [--format text|json] [--deny <RAS-ID>...] [--allow <RAS-ID>...]
```

Checks, keyed by the rules above:

| Diagnostic | Rule | Detection |
| --- | --- | --- |
| duplicate tile/sequence id | RAS-103 | discovery sets |
| statement in sequence body outside the allowed grammar (RAS-301 list) | RAS-301 | AST statement classifier over `#[sequence]` bodies |
| bare call resolving to a discovered tile/sequence | RAS-302 | `CallInfo` vs `call!` sites |
| call argument that is a computed expression | RAS-305 | argument expression classifier (identifier / literal / `select!`/`new!`/`storage!`/`.clone()` chain = ok) |
| binding meant as dataflow silently resolved to `External` | RAS-305 | `FlowResolver` output introspection — the "silent CFS degradation" detector |
| control-flow construct in sequence body | RAS-306 | AST |
| forbidden idiom in tile body: `std::` paths, `HashMap`/`HashSet`, float types/ops, `SystemTime`, `thread`, `env`, `rand` | RAS-206 | AST + import scan of the tile's module graph (heuristic, deny-listed idioms; not a purity proof) |
| whole-collection tile parameter over threshold without recur (`Vec<T>` parameter fed from an unsliced source) | RAS-208 | binding + type heuristic, warn-level |
| `main` returning an inline expression | RAS-501 | AST of `main`'s return position |
| entry argument without a manifest entry (when `input_manifest.json` present) | RAS-102 | manifest cross-check |

Output modes: human text with spans; `--format json` emitting rustc-style diagnostics
(file/range/severity/code/message) so editors and agents parse it identically.
Exit non-zero on any deny-level finding → CI-ready.

### Tier 3 — editor and agent integration

- **rust-analyzer, today, zero new code:** point check-on-save at the validator —
  ```jsonc
  // .vscode/settings.json / rust-analyzer config
  { "rust-analyzer.check.overrideCommand":
      ["cargo", "raster", "check", "--format", "json"] }
  ```
  (`cargo raster check` runs `cargo check` first and appends its own diagnostics, so
  users lose nothing.) Combined with Tier 1, this gives inline diagnostics for the
  entire rule set inside any rust-analyzer editor.
- **Agent loop:** the skill's check ladder inserts `cargo raster check` immediately
  after every edit batch (it is fast — parse + walk, no codegen). Optionally a Claude
  Code `PostToolUse` hook on `Edit|Write` for `**/src/**/*.rs` in Raster projects runs
  it automatically, making rule violations self-correcting within the agent turn.
- **Dedicated Raster LSP: explicitly deferred.** A separate language server would only
  add completions/hover for the DSL (e.g. `select!` path completion from `Selectable`
  schemas, `call_recur!` argument scaffolding). That is worth revisiting once the
  authoring surface stabilizes; the diagnostics problem — the verifiability-critical
  part — is fully covered by Tiers 1+2 without owning an LSP.

## 4. Rollout

1. **Phase 1 — rule set + skill.** Land this document; write the skill with rules
   marked `[none]` explicitly flagged as "manual review required, no tool catches
   this yet". Immediate value for agent-driven authoring. Update
   `docs/tile-authoring.md` to point at the RAS index instead of duplicating it.
2. **Phase 2 — Tier 1 macro diagnostics.** RAS-302/303/304/204/202/306 in-macro, with
   UI tests. Skill drops those from the manual-review list.
3. **Phase 3 — `cargo raster check` (Tier 2)** with the table above; wire
   `--format json`; add to CI and to the skill's check ladder; document the
   rust-analyzer `overrideCommand` setup in the README.
4. **Phase 4 — polish.** `select!`-path validation against `Selectable` schemas,
   chain-level type-compatibility checks (RAS-502), and the deferred LSP decision.

## 5. Files likely to change

- `docs/proposals/authoring-skill-and-tooling.md` — this document
- `.claude/skills/raster-authoring/**` — new skill (Phase 1)
- `docs/tile-authoring.md` — align with RAS rule IDs (Phase 1)
- `crates/raster-macros/src/lib.rs`, `crates/raster-macros/src/recur.rs` — spanned
  errors + new validations (Phase 2)
- `crates/raster/tests/ui/**` — new UI tests per validation (Phase 2)
- `crates/raster-compiler/src/validate.rs` — new validator module (Phase 3)
- `crates/raster-cli/src/main.rs`, `crates/raster-cli/src/commands.rs` — `check`
  subcommand (Phase 3)

## 6. Non-goals

- No general control-flow support in sequences — RAS-306 *forbids* what the CFS can't
  express; designing conditionals/branches is a separate proposal.
- No purity *proof* for tiles — RAS-206 checking is heuristic deny-listing; true
  enforcement is the zkVM replay itself.
- No standalone LSP in this proposal (deferred, §3 Tier 3).
- No changes to the trace/commitment/fraud-proof protocol — this is authoring-surface
  and tooling only.
