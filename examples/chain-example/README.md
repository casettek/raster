# `chain-example` — a three-stage raster chain

One computation split into three chained programs, each stage's authorized
output becoming the next stage's authorized input.

```text
  measurements + threshold                                    Report
        │                                                       ▲
        ▼            Filtered            Stats                  │
  ┌────────────┐  ───────────▶  ┌────────────┐  ─────────▶ ┌────────────┐
  │  phase 1   │                │  phase 2   │             │  phase 3   │
  │ normalize  │   output.bin   │ aggregate  │  output.bin │  report    │
  └────────────┘                └────────────┘             └────────────┘
```

| stage | project | does | shows |
| --- | --- | --- | --- |
| `normalize` | `phase1-normalize` | keep samples `>= threshold` | committed externals, output-building `call_recur!` |
| `aggregate` | `phase2-aggregate` | fold kept samples to `(count, sum, max)` | state-only `call_recur!`, scalar assembly tile |
| `report` | `phase3-report` | format the final report | `Draft<T>` threaded across tiles, integer-only math |

`Raster.toml` here holds a `[chain]` table and no `[program]` table — the
analogue of a Cargo *virtual workspace* manifest. The stage projects are
ordinary raster programs in subdirectories.

## Running it

```bash
cd examples/chain-example
cargo raster chain run --no-auth          # unattested, fast — the dev loop
```

This is the path the integration test covers and the one that works from a
clone. The **authenticated** path additionally needs each stage's program
identity, which is read from a `Raster.lock` (or a cached
`target/raster/program.bin`). Neither is checked in here, because generating
them means compiling every tile into a RISC0 guest — so build the stages first:

```bash
(cd phase1-normalize && cargo raster build)   # and phase2-, phase3-
cargo raster chain run                        # writes a chain-commitment
cargo raster chain audit                      # links + identities, no proving
```

Re-run a single stage in place, against the artifacts already on disk. Stages
after it are stale by definition, so they are deleted:

```bash
cargo raster chain run --no-auth --stage aggregate
#   invalidating 1 downstream stage: report
#   ▸ stage 2/3  aggregate   (phase2-aggregate)   (re-run in place)
```

Omit `--run` and it works inside the most recent run (`latest`); pass
`--run <dir>` to target an older one. See
`docs/proposals/chain-stage-execution.md`.

## Regenerating the committed inputs

`phase1-normalize`'s fixtures are checked in so the chain runs (and its
integration test passes) straight from a clone. To change the data:

```bash
cargo run -p chain-stage-normalize --features gen-input --bin gen_input -- \
  examples/chain-example/phase1-normalize
```

It prints the two structural commitments; paste them into the `normalize`
stage's `external` bindings in `Raster.toml`. The commitments are what
authorize the inputs, so they must move with the data.

## Why the phase 3 report is built the way it is

`build_report`-style tiles that take a whole value and emit a fully assembled
result are the shape the authoring rules exist to prevent: an unbounded amount
of work inside one replay unit, and a native function with `#[tile]` on it.
Phase 3 instead appends **one line per tile call** through a `Draft<Report>`,
so each replay unit is small and bounded, and the draft pays only for its
increment rather than re-committing the whole report every step.

The same rule is why phase 1 and phase 2 iterate with `call_recur!` — one
sample per replay unit — rather than looping inside a tile.
