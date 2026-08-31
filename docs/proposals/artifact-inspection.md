# Proposal: `artifact-inspection` — `cargo raster show`, reading a raster artifact back

Status: proposed (2026-08-21) — **rev 2** (2026-08-31): adds §6 `--show-output` on `run` /
`chain run`, resolving open question 3; adds §4.1 stating that the text format is `Debug`-shaped
but cannot name a struct, because `RasterNodeKind::Struct` records field names and no type name.
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
    pub max_depth: usize,            // default 32
}

/// Decode an artifact from its payload and index.
pub fn read_raster_value(
    data_path: &Path,
    index_path: &Path,
    limits: &ReadLimits,
) -> Result<RasterValue>;

/// Decode from the payload alone — structure and field names, untyped leaves.
pub fn read_raster_value_structural(data: &[u8], limits: &ReadLimits) -> Result<RasterValue>;
```

`RasterValue` is a **new, deliberately public** type rather than making `TreeValue` public.
`TreeValue` is the encoder's internal representation and is load-bearing for commitments; pinning
it as public API would freeze an internal on a rendering use case. The conversion is mechanical
and lives beside the walk.

Limits are a parameter, not a constant, because the same walk serves a terminal (truncate hard)
and `--format json` piped into a file (truncate loosely or not at all).

### 2. The structural fallback matters more than it looks

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

Where a `0x09` handle's stored root disagrees with its own elements, say so on that node. Nothing
outside a selection proof catches that today.

### 4. CLI surface

```bash
cargo raster show <artifact>                    # index inferred: <artifact> with .rindex
cargo raster show <artifact> --index <path>
cargo raster show <artifact> --select report.lines[0]
cargo raster show <artifact> --format text|json
cargo raster show <artifact> --max-bytes 4096 --max-list 1000 --depth 8
```

- **Index inference** mirrors the chain runner's own default (`chain.rs:1023`: `path` with the
  extension replaced by `rindex`), so `cargo raster show output.bin` just works on a stage
  directory. Missing index → structural mode, with a one-line note saying which mode ran and why.
  Never silent.
- **`--select`** reuses the existing selector-path machinery that `tree_value_from_raster_location`
  already implements, including list ranges. Inspecting one field of a large artifact should not
  require decoding all of it — and the index makes that genuinely cheap, since the walk is
  node-addressed rather than a linear scan.
- **`--format json`** so it composes with `jq` and with tests. Text is for humans and is the
  default.

### 5. Chain ergonomics

The forcing case is a chain stage, so name one:

```bash
cargo raster chain show <stage>          # the stage's output in the latest run
cargo raster chain show <stage> --run <dir>
```

Resolution reuses `chain-stage-execution`'s `latest` pointer and `--run` exactly, so the two
commands agree on what "the current run" means. This is sugar over
`cargo raster show <run>/<stage>/output.bin`, and is worth it only because that path is the one
nobody wants to type while iterating.

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
- **Teaching `chain run` to print stage outputs inline.** Rejected as the primary answer — it only
  helps at run time, which is exactly the limitation being removed, and it would put a truncation
  policy in the middle of the runner. `chain show` after the fact composes better.

## Implementation plan

1. **Split walk from hash** in `crates/raster-core/src/input.rs`: `parse_subtree_root` becomes a
   visitor-driven traversal with the current hashing as one visitor. Behaviour-preserving; the
   existing selection-proof tests are the guard.
2. **`RasterValue` + `ReadLimits` + the two `read_raster_value*` entry points** in
   `raster-runtime`, converting from `TreeValue` and from the structural walk. Promote
   `RasterIndex::from_bytes` and the `tree_value_from_raster_node` path to `pub`.
3. **Renderers** — text tree and JSON — in `raster-cli`, with truncation markers that are visibly
   truncation (`… 936 more elements`), never silent elision.
4. **`cargo raster show`** with index inference, `--select`, `--format`, and the limit flags; the
   commitment line of §3.
5. **`cargo raster chain show`** over `latest` / `--run`.

Steps 1–2 are the proposal; 3–5 are surface.

## Verification

- **Round-trip.** For each of `hello-tiles`' and `examples/chain-example`'s artifacts:
  `write_raster_files(v)` then `read_raster_value` yields a `RasterValue` matching `v` field for
  field, including a `List`, a nested struct, and a `String`.
- **Every tag.** A fixture per payload tag `0x00`–`0x0B`, including the `0x09` handle, `0x0A`
  metadata, and a `0x0B` page — the tag table is the checklist, and a new tag without a `show`
  arm should be a visible omission.
- **Structural mode agrees with typed mode** on structure and field names for the same artifact,
  differing only in leaf rendering.
- **Integrity.** A byte-flipped `output.bin` reports a commitment mismatch rather than rendering
  confidently; a `0x09` handle whose stored root disagrees with its elements is flagged.
- **Limits hold.** A `Bytes<262144>` page and a 100k-element list both render bounded, in bounded
  time, with the truncation stated.
- **Chain.** `cargo raster chain show report` on `examples/chain-example` prints the same value the
  stage printed at run time — the specific gap in §Problem, closed.

## Open questions

- **Should `show` verify the commitment by default, or on `--verify`?** Free here (the bytes are
  in hand), but it makes a viewer into a checker, and a mismatch then has to decide between a
  warning and a non-zero exit. Defaulting to *report, exit 0* and putting the exit-code behaviour
  behind a flag is the current leaning.
- **Does `--select` belong in v1?** It is the difference between "read a file" and "query a file",
  and the machinery exists. But every selector-path surface is one more place the path grammar can
  drift from `select!`'s.
- ~~**Should `run` and `chain run` print the output value by default once this exists?**~~
  **Resolved 2026-08-31 → §6**: opt-in `--show-output`, off by default, final stage only for a
  chain. The residue is narrower — whether `--show-output=all` is worth defining for a chain, and
  whether a chain long enough to want it is better served by `chain show` in a loop.

## Out of scope

- Writing or editing artifacts. `show` is read-only; there is no `cargo raster edit`, and the
  commitment is why.
- Rendering a *trace* (`trace.bin`). It holds per-step input/output witnesses and is the natural
  next inspection target, but it is a different artifact with a different shape — a sibling
  proposal, not a flag on this one.
- Exact Rust `Debug` rendering via the program crate (the linking alternative above).
- Anything about `Raster.lock` / `program.bin` inspection; `cargo raster program` already covers
  program identity.
