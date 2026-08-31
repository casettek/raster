# Proposal: `artifact-inspection` — `cargo raster show`, reading a raster artifact back

Status: **implemented** (2026-08-31) — rev 2, minus the deferred §2 structural fallback. See
§Implementation record for what shipped and where it departs from the design below.

Rev 2 (2026-08-31): adds §6 `--show-output` on `run` / `chain run`, resolving open question 3;
adds §4.1 stating that the text format is `Debug`-shaped but cannot name a struct, because
`RasterNodeKind::Struct` records field names and no type name. **Rejects `--select` and
`cargo raster chain show`** (§Alternatives), and **defers** the structural fallback (§2) with
the `raster-core` refactor under it.

`cargo raster show <artifact>` is unchanged and remains the primary answer — the durable
artifact stays readable without re-running anything. `--show-output` is the same reader invoked
at the end of a run, on the artifact that run just wrote; it removes the keystrokes from the
fast loop without becoming the only way in. One implementation, two entry points: the flag
supplies paths it already knows, so it is sugar and not a second code path.

Companion to: [`program-end.md`](./program-end.md) (implemented) — defines the output artifact
this reads; [`chain-stage-execution.md`](./chain-stage-execution.md) (partly implemented) — the
per-stage dev loop that makes the absence acute.
Related: [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) (half landed) owns
`cargo raster check`. That is *validation* of source; this is *inspection* of data. Sibling
commands, neither subsumes the other.

## Problem

There is no way to read a raster artifact back.

`cargo raster run` prints the path of `output.bin` (`crates/raster-cli/src/commands/run.rs:261`)
and stops. `cargo raster chain run` prints two truncated hashes per stage
(`chain.rs:334`). `output_manifest.json` holds a commitment and nothing else
(`crates/raster-runtime/src/input.rs:2694`). The public API has
`write_raster_files` / `encode_raster_value` (`crates/raster/src/input.rs:2959`, `:2966`) with
**no decode counterpart**.

So the current state of the art for "what did that stage actually produce" is:

```console
$ strings -n 4 target/raster/chains-no-auth/latest/report/output.bin
title-
Pipeline report for sensor-A
lines
count   : 6
sum     : 353
max     : 88
mean    : 58.83
```

This works only because struct field names and `String` leaves are stored as length-prefixed
UTF-8. Integers are little-endian binary and invisible to it; structure is gone; a `Bytes<P>`
region is noise. And it is `strings(1)` — reaching for it at all is the finding.

The gap costs most where iteration is fastest. `chain-stage-execution` exists so a developer can
re-run one stage of a long chain; having re-run it, the only way to see what changed is to have
had the foresight to `raster::println!` it, because the artifact on disk is unreadable. Two of the
three ways to inspect a value (stdout, hashes) are available only *while* the run happens; the
third is `strings`.

## Goal

`cargo raster show <artifact>` — print the value in a raster artifact, typed and structured.

One command over every raster payload, because they are one format: a program's `output.bin`, an
external input's `*.rastered`, a chain stage's artifact. Whatever produced it, the bytes and the
index are the same shape.

And `--show-output` on `run` / `chain run` (§6) — the same reader on the artifact the run just
wrote, so the common case does not require knowing a path. Two entry points, one implementation.
`show` is the capability; `--show-output` is the ergonomics. Neither is a substitute for the
other: a flag alone would make a durable artifact readable only at the moment it was produced,
and a command alone would make the fast loop retype a path it already printed.

## Facts the design builds on (verified in code)

**The decoder already exists, fully, and is exercised on every selection.** This is the finding
that sizes the proposal — it is not a decoder to write, it is a decoder to expose.

- **`.rindex` is a typed, self-describing index.** `RasterNodeKind`
  (`crates/raster-runtime/src/raster_index.rs:31`) is `Unit | Leaf { type_name } | Struct { fields }
  | List { len, elements, merkle_levels } | Map | EnumUnit { variant } | …`. **A leaf carries its
  Rust type name.** There is no schema to reconstruct and no schema file to invent: the artifact
  says what its own leaves are.
- **`parse_leaf_value(type_name, bytes)`** (`crates/raster-runtime/src/input.rs:1127`) already
  dispatches on that name to produce a typed value — `bool`, the integer widths, `String`,
  `BytesPage` — and rejects malformed and trailing-byte payloads.
- **`tree_value_from_raster_node(index, data, node_id)`** (`:1328`) already walks index + payload
  into a complete `TreeValue`. `tree_value_from_raster_location` (`:1271`) does the same for a
  selection, including list ranges.
- **`RasterIndex::from_bytes`** parses the `.rindex` (used at
  `crates/raster-runtime/src/source/file.rs:236`), and `SourceFile` already implements the
  `RasterData` reader the walk needs.
- **`typed_value_from_tree::<T>`** (`:1118`) converts a `TreeValue` back into a Rust value — the
  exact inverse of `tree_value_from_serialize` (`:1109`) that the encoder uses.
- **Every one of those is `pub(crate)` inside `raster-runtime`.** Nothing in `raster-cli` can
  reach them. The missing piece is visibility, not capability.
- **The payload alone is structurally self-describing, without the index.**
  `parse_subtree_root` (`crates/raster-core/src/input.rs:624`) walks the whole tag space —
  `0x00` leaf, `0x01` struct (with inline UTF-8 field names), `0x02` list, `0x03` unit, `0x04` map,
  `0x05`–`0x08` enum forms, `0x09` list handle, `0x0A` list metadata, `0x0B` bytes page — and the
  tag table is centrally allocated in that doc comment. It discards everything but hashes.
- **`payload_structural_root`** (`crates/raster-core/src/input.rs:850`) is already public and
  recomputes an artifact's commitment from its bytes — so a viewer can state whether what it is
  showing is what was committed.
- **`0x09` list handles carry a stored root and their elements both.** `parse_subtree_root`
  returns the stored root and skips the body; a renderer sees the elements. The two can disagree,
  and today nothing outside a selection proof would notice.
- **`0x0B` bytes pages are page-sized by construction** — `paged-bytes.md` uses 256 KiB in its
  worked example. Any renderer that does not truncate is a renderer that will one day print a
  gigabyte.

## Design

### 1. `raster-runtime`: one public entry point

```rust
/// A decoded raster value — the rendering-facing view of a `TreeValue`.
pub enum RasterValue {
    Unit,
    Bool(bool),
    Int(i128),          // every signed/unsigned width, widened
    Float(f64),
    Str(String),
    Bytes { index: u64, offset: u64, len: u64, data: Vec<u8> },
    Struct(Vec<(String, RasterValue)>),
    List { len: u64, elements: Vec<RasterValue>, truncated: bool },
    Map(Vec<(RasterValue, RasterValue)>),
    Enum { variant: String, payload: Option<Box<RasterValue>> },
}

pub struct ReadLimits {
    pub max_bytes_per_leaf: usize,   // default 256
    pub max_list_elements: usize,    // default 64
    pub max_struct_fields: usize,    // default 256
    pub max_depth: usize,            // default 32
}

/// Decode an artifact from its payload and index.
pub fn read_raster_value(
    data_path: &Path,
    index_path: &Path,
    limits: &ReadLimits,
) -> Result<RasterValue>;

/// Decode from the payload alone — structure and field names, untyped leaves.
/// DEFERRED (rev 2, §2) — not in the first cut.
pub fn read_raster_value_structural(data: &[u8], limits: &ReadLimits) -> Result<RasterValue>;
```

`RasterValue` is a **new, deliberately public** type rather than making `TreeValue` public.
`TreeValue` is the encoder's internal representation and is load-bearing for commitments; pinning
it as public API would freeze an internal on a rendering use case. The conversion is mechanical
and lives beside the walk.

Limits are a parameter, not a constant, because the same walk serves a terminal (truncate hard)
and `--format json` piped into a file (truncate loosely or not at all).

### 2. The structural fallback matters more than it looks — deferred

> **Rev 2: deferred, not dropped.** `--show-output` never needs it: the run wrote the `.rindex`
> seconds earlier, so the index is present by construction. Only standalone `show` can be pointed
> at an orphaned payload. Deferring keeps the first cut inside `raster-runtime` + `raster-cli` and
> avoids refactoring commitment-critical code in `raster-core` to serve a degraded mode. Until it
> lands, `show` with no index errors with the path it looked for and says structural mode is not
> implemented — never a silent or partial render. The argument below stands for when it is built.

`read_raster_value_structural` takes the payload with **no index**. It recovers structure and
field names, and renders leaves as raw bytes:

```
Report {
  title: <leaf 28B> "Pipeline report for sensor-A"
  lines: [4] { <leaf 11B> "count   : 6", … }
}
```

Worth having because the index is the artifact most likely to be missing — a payload gets copied,
pasted into a bug report, or fished out of a chain run whose `.rindex` was not kept. Degrading to
"structure without types" is far better than refusing.

**It must reuse `parse_subtree_root`'s traversal, not re-implement it.** A second parser over the
tag space is a second thing that can disagree with the commitment — a viewer that shows you
something other than what was committed is worse than no viewer. The refactor is to split that
function into a walk plus a visitor, with the existing hash computation as one visitor and
rendering as the other. One traversal, two consumers, and the tag table keeps its single home.

### 3. Integrity comes free, so take it

The walk already has the bytes; `payload_structural_root` is already public. So `show` recomputes
the root and reports it:

```
commitment 10a07855108e669e…  ✓ matches output_manifest.json
```

Three outcomes: matches the sibling manifest, no manifest present (state the root, claim nothing),
or **mismatch** — which is a corrupt or swapped artifact and should be loud. This makes `show` the
natural first command when a chain link fails, rather than a separate debugging step.

**Loud means both formats and the exit status** (resolved 2026-08-31; see §Open questions). Text
mode prints the report on stdout. JSON mode prints it on **stderr**, so stdout stays a single
parseable document — putting it in the JSON would either change the value's shape unpredictably
or bury the warning in the data. Either way a mismatch exits non-zero. The value still renders
first: seeing what a corrupt artifact decodes to is the reason to point `show` at one.

Where a `0x09` handle's stored root disagrees with its own elements, say so on that node. Nothing
outside a selection proof catches that today.

### 4. CLI surface

```bash
cargo raster show <artifact>                    # index inferred: <artifact> with .rindex
cargo raster show <artifact> --index <path>
cargo raster show <artifact> --format text|json
cargo raster show <artifact> --max-bytes 4096 --max-list 1000 --depth 8
```

- **Index inference** mirrors the chain runner's own default (`chain.rs:1023`: `path` with the
  extension replaced by `rindex`), so `cargo raster show output.bin` just works on a stage
  directory. Missing index → the §2 error, naming the path it looked for. Never silent.
- **`--format json`** so it composes with `jq` and with tests. Text is for humans and is the
  default.
- **No `--select`.** Rejected in rev 2 — see §Alternatives. `show` reads a file; it does not
  query one.

### 4.1 The text format is `Debug`-shaped, and cannot name a struct

The obvious rendering target is Rust's `Debug`, and the index gets most of the way there — but
not all of it. `RasterNodeKind::Leaf` carries `type_name`, so a leaf can be rendered as its Rust
type. `RasterNodeKind::Struct` carries `fields: Vec<RasterStructField>` and **no type name**
(`crates/raster-runtime/src/raster_index.rs:36`). The artifact knows a struct's field names and
not what the struct is called.

So the text format brackets structs anonymously, and does not pretend otherwise:

```
{
  title: "Pipeline report for sensor-A"
  lines: [4] [
    "count   : 6"
    "sum     : 353"
    … 2 more elements
  ]
  total: 353u64
}
```

Enum variants *are* named (`EnumUnit`/`EnumNewtype`/`EnumTuple`/`EnumStruct` all carry
`variant`), so they render as `Variant { … }`. This asymmetry — variants named, structs not — is
a property of the index, not a choice. Recovering the struct name needs the declared type, which
is the linking alternative in §Alternatives, still future work.

### 5. Integrity on the `show` path

Covered by §3. Worth noting that the chain runner already does half of it: `collect_output`
(`chain.rs:2129`) recomputes the structural root from `output.bin` and hard-errors if it
disagrees with `output_manifest.json` (`chain.rs:2144`). So a chain stage's artifact is already
cross-checked before `--show-output` ever renders it; the §3 commitment line is new work only on
the standalone `show` and plain `run` paths.

### 6. `--show-output` on `run` and `chain run`

```bash
cargo raster run --show-output
cargo raster chain run --show-output
cargo raster chain run --no-auth --stage report --show-output
```

Opt-in, off by default. It runs the program, then renders the `output.bin` it just wrote, using
§1's reader and §4's renderer with `ReadLimits::default()`.

- **`run`** renders at `run.rs:255`, inside the existing `output_manifest.json` existence check
  that already gates the `Program output artifact:` block. Under a distinct header — `run.rs:238`
  already prints `Output:` for `raster::println!` lines, and the two must not read as one block.
- **`chain run`** renders the **final stage's** output. Combined with `chain-stage-execution`'s
  `--stage`, that is also how you inspect a middle stage: `--stage report --show-output` re-runs
  that stage and shows it, because the stage you re-ran is the last one that ran. So "final stage
  only" costs nothing in the dev loop it was designed for, and keeps a 74-stage chain from
  printing 74 values. `--show-output=all` is left as residue in §Open questions.
- **It reads from disk, like everything else.** `cargo raster run` spawns the program as a child
  process (`run.rs:222`) and recovers `raster::println!` output by scraping `[output]`-prefixed
  stdout lines; the CLI never holds the output value in memory. `chain run` likewise re-reads the
  artifact through `collect_output` (`chain.rs:581`). So `--show-output` is the same decode as
  `show`, triggered at a different moment — not a shortcut past it. This is why the two surfaces
  can share one implementation rather than merely one renderer.
- **No truncation policy in the runner.** `ReadLimits` and its defaults live on `raster-runtime`'s
  public API (§1); the runner passes `ReadLimits::default()` and holds a call, not a policy. This
  was the second half of the original objection to inline printing, and it is what makes the flag
  admissible now.

## Alternatives considered

- **A `schema.json` artifact beside `program.bin`**, generated from `Selectable::schema()` and
  hash-checked against `InterfaceDecl.schema_hash` (`crates/raster-core/src/program.rs:50`). This
  was the design until the `.rindex` was read closely: `RasterNodeKind::Leaf` already carries
  `type_name`, so the artifact is self-describing and a schema side-car would be a second source
  of truth for something the data already states. Rejected — but the observation that
  `schema_hash` pins a schema without carrying one is real, and will matter to anything that needs
  the *declared* type rather than the *encoded* one.
- **Linking the program crate to recover types** (a `--project` flag building a helper binary that
  calls `typed_value_from_tree::<T>`, the way `gen_input` works). Gives exact Rust `Debug` output.
  Rejected for v1: it needs a toolchain and a build to look at a file, and `type_name` from the
  index already gets ~all of the way there. Left as future work for the case where the *exact*
  Rust rendering matters.
- **Making `TreeValue` public** instead of adding `RasterValue`. Rejected in §1 — it freezes an
  encoder internal on a rendering use case.
- **Extending `cargo raster analyze`** rather than a new command. Rejected: `analyze` reads traces
  and reports cycles and sizes. Different input, different question.
- **Teaching `chain run` to print stage outputs inline.** Rejected as the *primary* answer, and
  that still holds: a run-time-only view leaves the durable artifact readable only through an
  ephemeral event, so an `output.bin` from yesterday's run would need a re-run to look at a file
  already on disk. **Rev 2 accepts it as a secondary answer** — `--show-output`, §6 — because the
  two objections do not survive the flag being sugar over `show`. "Only helps at run time" is
  answered by `show` existing; "truncation policy in the runner" is answered by `ReadLimits`
  living on the runtime API. Adopted as an addition, never as a replacement.
- **`--select <path>` on `show`.** Rejected in rev 2. The machinery exists —
  `tree_value_from_raster_location` (`raster-runtime/src/input.rs:1271`) already resolves selector
  paths including list ranges — so this is a rejection on surface area, not on cost. Two reasons.
  Every selector-path surface is another place the path grammar can drift from `select!`'s, and
  that grammar is load-bearing for selection proofs, not just for reading. And it changes what the
  command *is*: `show` reads a file, and `--select` makes it query one, which invites the whole
  query-language tail. `--format json` piped to `jq` covers the real need with a grammar that is
  somebody else's problem. Revisit when the truncation limits are demonstrably the thing standing
  between a developer and a field they need.
- **`cargo raster chain show <stage>`.** Rejected in rev 2. It was sugar over
  `cargo raster show <run>/<stage>/output.bin`, justified purely by that path being tedious to
  type. With `--show-output` on `chain run` covering the iterate-and-look loop — including the
  `--stage` case — what is left is a second command that resolves `latest`/`--run` and then calls
  `show`. That is a second place chain run-resolution can drift from `chain-stage-execution`'s,
  bought for keystrokes. Type the path.

## Implementation plan

1. **`RasterValue` + `ReadLimits` + `read_raster_value`** in `raster-runtime`, converting from
   `TreeValue`. Promote `RasterIndex::from_bytes` (`raster_index.rs:135`) and the
   `tree_value_from_raster_node` path (`input.rs:1328`) to `pub`.
2. **Renderers** — text tree (§4.1) and JSON — in `raster-cli`, with truncation markers that are
   visibly truncation (`… 936 more elements`), never silent elision.
3. **`cargo raster show`** with index inference, `--format`, and the limit flags; the commitment
   line of §3; the §2 error when the index is absent.
4. **`--show-output`** on `run` (`run.rs:255`) and `chain run` (`chain.rs:581`), final stage only.

Step 1 is the proposal; 2–4 are surface. Nothing here touches `raster-core`.

**Deferred** — `read_raster_value_structural` and the walk/hash split in
`crates/raster-core/src/input.rs` (`parse_subtree_root` becomes a visitor-driven traversal with
the current hashing as one visitor, behaviour-preserving, guarded by the existing selection-proof
tests). Only standalone `show` on an orphaned payload needs it; see §2.

## Verification

- **Round-trip.** For each of `hello-tiles`' and `examples/chain-example`'s artifacts:
  `write_raster_files(v)` then `read_raster_value` yields a `RasterValue` matching `v` field for
  field, including a `List`, a nested struct, and a `String`.
- **Every tag.** A fixture per payload tag `0x00`–`0x0B`, including the `0x09` handle, `0x0A`
  metadata, and a `0x0B` page — the tag table is the checklist, and a new tag without a `show`
  arm should be a visible omission.
- **Missing index.** `show` on a payload whose `.rindex` is absent errors, names the path it
  looked for, and renders nothing partial (§2, until the structural mode lands).
- **Integrity.** A byte-flipped `output.bin` reports a commitment mismatch rather than rendering
  confidently; a `0x09` handle whose stored root disagrees with its elements is flagged.
- **Limits hold.** A `Bytes<262144>` page and a 100k-element list both render bounded, in bounded
  time, with the truncation stated.
- **The two surfaces agree.** On `examples/chain-example`,
  `cargo raster chain run --no-auth --show-output` and
  `cargo raster show <run>/report/output.bin` print byte-identical renderings. This is the test
  that keeps `--show-output` sugar rather than a second code path.
- **Chain.** That same `--show-output` run prints the value the `report` stage would otherwise
  have had to `raster::println!` — the specific gap in §Problem, closed.
- **`--stage` inspection.** `chain run --no-auth --stage report --show-output` on a three-stage
  chain shows `report`'s output and no other stage's, confirming "final stage of this run" is the
  right rule for the dev loop (§6).

## Open questions

- ~~**Should `show` verify the commitment by default, or on `--verify`?**~~ **Resolved
  2026-08-31: verify by default, report in both formats, and exit non-zero on a mismatch** —
  against the "report, exit 0" leaning recorded here. What settled it was building `--format
  json`: a viewer that renders a corrupt artifact and exits 0 is fine for a human, who reads the
  commitment line, and wrong for a script, which reads the exit status and gets "success" for an
  artifact that is not what was committed. Exit 0 made the machine-readable path the quiet one.
  A `--no-verify` escape hatch was not added; nobody has wanted one yet.
- ~~**Does `--select` belong in v1?**~~ **Resolved 2026-08-31: no, and not in v2 either** — see
  §Alternatives. Not deferred but rejected; reopening it means arguing that `--format json | jq`
  is insufficient, which is a different and harder claim than "the machinery already exists".
- ~~**Should `run` and `chain run` print the output value by default once this exists?**~~
  **Resolved 2026-08-31 → §6**: opt-in `--show-output`, off by default, final stage only for a
  chain. The residue is narrower — whether `--show-output=all` is worth defining for a chain, or
  whether a chain long enough to want it is better served by `show` over the run directory, which
  needs no flag and no re-run.

## Implementation record (2026-08-31)

Steps 1–4 of the plan landed. Files: `crates/raster-runtime/src/reader.rs` (new),
`RasterValue` + the bounded walk in `crates/raster-runtime/src/input.rs`,
`crates/raster-cli/src/commands/show.rs` (new), and the flag wiring in
`commands/run.rs` and `chain.rs`. Nothing in `raster-core` was touched, as §2 predicted.
Tests: 12 in `reader.rs`, 9 in `crates/raster-cli/tests/show_cli.rs`.

**Where the code departs from §1's sketch.** All of these are the same cause — *"never silent
elision"* needs a place to record that something was cut, and the sketch had nowhere to put it:

- `Str { value, truncated }`, `Bytes { …, truncated }`, `Map { len, entries, truncated }` and
  `Struct { len, fields, truncated }` carry truncation flags. The sketch had a bare `Str(String)`,
  `Map(Vec<…>)` and `Struct(Vec<…>)`, which can only truncate silently.
- **`max_struct_fields` was added to `ReadLimits`** (default 256) and `Struct` became bounded.
  §1's sketch bounded lists, bytes and depth but let a struct's field walk run to completion,
  on the implicit assumption that a struct's width is fixed by the Rust type it came from. It is
  not: the field table is read out of the `.rindex`, so its width is *data*, and a corrupt or
  hostile index can declare any number of fields. The limit is deliberately far above
  `max_list_elements` — it guards against a malformed index rather than expressing a display
  preference, so it should never fire on a real type. `EnumStruct` shares the same walk and is
  covered by construction.
- `Int { value, ty }` carries the Rust type name, because §4.1 renders `353u64` and the width is
  lost once every leaf is widened to `i128`.
- `Elided` was added: `max_depth` needs a representation in the value, not just in the renderer.
- **`Float` was dropped.** It is not a gap — `TreeValue` has no float variant and
  `parse_leaf_value` has no float arm, so the encoder cannot produce one. A `Float` in the reader
  would describe a value that cannot exist.

**Other departures.**

- **`read_raster_artifact` / `RasterArtifact` sit beside `read_raster_value`.** §1's signature
  returns only the value, but §3 needs the roots; rather than change the documented signature,
  the integrity-carrying form is a second entry point and `read_raster_value` delegates to it.
- **The walk is its own traversal, not a conversion from `TreeValue`.** §Implementation plan said
  "converting from `TreeValue`", which would materialize the whole tree before truncating it —
  exactly the 100k-element list the limits exist to avoid. `raster_value_from_node` mirrors
  `tree_value_from_raster_node` node for node and reuses `parse_leaf_value` verbatim, so the leaf
  semantics stay literally shared with the encoder.
- **`--format json` defaults to unbounded**, via `ReadLimits::unbounded()`. §1 said JSON should
  "truncate loosely"; a pipe into `jq` has no reason to truncate at all, and the limit flags still
  apply if given.
- **A mismatch exits non-zero, in both formats**, and the JSON report goes to stderr — §3 and
  §Open questions, both amended. The first cut had the JSON arm render the value and return `Ok`
  without consulting the integrity result at all, which made the machine-readable path the one
  that could silently accept a corrupt artifact.
- **`--max-fields`** joins `--max-bytes` / `--max-list` / `--depth` on the CLI.
- **`--show-output` after a failed run** says the program failed rather than that it returns unit.
  Both produce no artifact; only one is a property of the program.

**Not implemented, and not merely deferred with §2:** the `0x09` cross-check from §3's last
paragraph — flagging a list handle whose *stored* root disagrees with its own elements. Whole
artifact integrity is in (payload structural root vs. the `.rindex` root, reported on every
`show` and `--show-output`), but the handle-level check is a statement about the payload's
internal consistency and needs the payload walk that §2 defers. It rides with the structural
fallback.

## Out of scope

- Writing or editing artifacts. `show` is read-only; there is no `cargo raster edit`, and the
  commitment is why.
- Rendering a *trace* (`trace.bin`). It holds per-step input/output witnesses and is the natural
  next inspection target, but it is a different artifact with a different shape — a sibling
  proposal, not a flag on this one.
- Exact Rust `Debug` rendering via the program crate (the linking alternative above).
- Anything about `Raster.lock` / `program.bin` inspection; `cargo raster program` already covers
  program identity.
