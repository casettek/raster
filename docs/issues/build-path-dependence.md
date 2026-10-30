# Issue: `build-path-dependence` — where a project and its `raster` checkout live changes what gets built, and sometimes whether it builds

Status: **open** 2026-10-30. Unowned. Two defects with one cause: the build and program identity
read filesystem paths that the source does not determine. §1 is a completeness defect with a
deterministic reproducer. §2 is a protocol defect that limits who can defend a challenge. §3 is a
smaller relocation gap in run artifacts.

Reproduced on `replay-profile` at main `7b8230c`. Every citation is committed code.

Related:
- [`stage-challenge.md`](../proposals/stage-challenge.md) — open question 6 and §Verification
  ("Anyone defends") record §2's consequence: a defender at another checkout path fails with
  `program identity drift`. That proposal places the fix in program identity, not in its own
  protocol; this issue is where that fix is tracked.
- [`program-identity.md`](../proposals/program-identity.md) — its §Verification lists
  "two docker-mode builds from different checkout paths produce equal tile image ids" as a goal
  (`:466`). Nothing in the default build path delivers it today (§2).
- [`tile-cost-estimator.md`](../proposals/tile-cost-estimator.md) §6 step 3 — the planned
  `run.json` stores absolute paths, which adds to §3.

## 1. The guest builder cannot find `raster` for some valid projects, and falls back to a path that never exists

**What happens.** `cargo raster build --backend risc0` and `cargo raster run-tile --backend
risc0` fail to build any tile guest for a project whose native build succeeds:

```
Runtime error: Failed to build guest for tile 'advance_line_cursor': Compilation failed:
error: failed to load manifest for dependency `raster`
  failed to read `/crates/raster/Cargo.toml`
  No such file or directory (os error 2)
```

**Why.**

1. `GuestBuilder::with_user_crate` (`raster-backend-risc0/src/guest_builder.rs:106`) reads only the
   **project's own** `Cargo.toml`. It takes the first line that contains both `raster` and `path`
   (`extract_raster_path`, `:155-169`, test at `:158`). Lines are scanned as text, not parsed as
   TOML.
2. The result is canonicalized with `.canonicalize().ok()` (`:115`), so a missing directory
   becomes `None` silently. The workspace root is then assumed to be two levels above the crate
   (`:117`).
3. With no workspace found, the generated guest `Cargo.toml` falls back to the relative path
   `../../../../crates/raster` (`:258-259`). It is relative to the generated guest crate. That
   crate always lives in a fresh temp directory, `<tmp>/.tmpXXXX/guest-<tile>`
   (`risc0.rs:239-242`). Four levels up from there is `/`, so **the fallback resolves to
   `/crates/raster` in every case** and can never succeed.

So any `raster` dependency the line scan does not find breaks the guest build, with an error
naming a path that appears nowhere in the project. Cases that reach it:

- **Workspace inheritance** (reproduced): the member declares
  `raster = { workspace = true, default-features = false }`, and the path lives in the workspace
  root's `Cargo.toml`, which is never read. Cargo and the native build accept it.
- **A path that no longer exists** (reproduced): for example a dependency into a removed git
  worktree. The native build fails too, but with cargo's correct message. The guest build reports
  `/crates/raster` instead.
- **A symlink in the `raster` path** (reproduced 2026-10-30, iris `prefill-range` with
  `../../../raster` pointing at a symlink to the checkout). Here the scan *does* find the path,
  but `:115` canonicalizes it, so the generated guest names `raster` by its real location. The
  user crate, a dependency of the same guest, still reaches `raster` through the symlinked
  path. Cargo keeps paths as written, so it sees two packages and refuses:
  ```
  error: package collision in the lockfile: packages raster v0.1.0 (…/bench-iris/rp/raster/crates/raster)
  and raster v0.1.0 (…/raster-project/raster/crates/raster) are different, but only one can be
  written to lockfile unambiguously
  ```
  The native build works.
- **Not covered by the scan, by reading:** a `raster` from git or a registry (no `path`), and any
  layout where `raster` is not two levels below the workspace root.
- **Matched by accident:** the `[dependencies.raster]` table form, whose `path = "…"` line does
  not mention `raster`, still works. It works only because the path string itself contains
  `raster` (`…/raster/crates/raster`). Any other first line that happens to contain both words,
  such as a dependency on another `raster-*` crate listed earlier, is taken instead.

**A second effect of the same `None`.** The tile cache key, `sources_fingerprint` (`:342`),
hashes the raster workspace's `Cargo.toml` and the `raster`, `raster-core` and `raster-macros`
sources only `if let Some(workspace) = &self.raster_workspace` (`:345`). When the path is not
found, those sources drop out of the key. A build that does succeed could then serve a cached
ELF built against other `raster` sources. This follows from reading the code; it has not been
reproduced.

**Reproducer (workspace inheritance, about 1 minute).**

1. Copy `examples/hello-tiles` without `target/` into `ws/app`, and copy the repository's
   `Cargo.lock` into `ws/`.
2. In `app/Cargo.toml`:
   - inline `serde`/`serde_json` (the example inherits them from this repository's workspace);
   - replace the `raster` line with `raster = { workspace = true, default-features = false }`.
3. Write `ws/Cargo.toml`:
   ```toml
   [workspace]
   members = ["app"]
   resolver = "2"

   [workspace.dependencies]
   raster = { path = "<checkout>/crates/raster", default-features = false }
   ```
4. In `ws/app`: `cargo build` succeeds. `cargo-raster raster build --backend risc0` fails with
   the error above.

## 2. Tile image ids depend on the absolute checkout path and `$CARGO_HOME`

**What happens.** The same source, at the same commit, built at two different paths gets
different tile image ids, and so a different `program_commitment` in `Raster.lock`.

**Why.** Tile ELFs carry absolute source paths, from panic locations. The `mac_weight_page`
guest of iris `prefill_range_l0` contains 10 such strings: paths under the `raster` checkout
(`/home/<user>/dev/…/crates/raster-core/…`) and under `$CARGO_HOME`
(`/home/<user>/.cargo/registry/src/index.crates.io-…/risc0-zkp-3.0.5/…`). The guest rustflags
(`guest_rustflags`, `guest_builder.rs:27`) have no `--remap-path-prefix`. The image id is a hash
of the ELF, so it inherits both paths.

**Consequences, observed:**
- **Defending a challenge.** From a second worktree at the same commit, a defender's re-run and
  walk succeed, then proving stops with `program identity drift` (`stage-challenge.md`
  §Verification, open question 6). "Anyone may defend" holds only for a defender whose build
  reproduces the claimer's paths.
- **Committed locks drift.** A `Raster.lock` generated in one checkout does not match another
  checkout's build. The mismatch only shows at prove time, as `program identity drift`, because
  `run --commit` does not check it. Seen 2026-10-20 on `examples/hello-tiles/Raster.lock`.
- **Diagnosis gets harder.** A lock difference after an edit looks like the edit moved tile ids.
  The way to check is to `cmp` the two ELFs built at the *same* path.

## 3. Run artifacts store absolute paths

A run directory cannot be moved or handed to another machine as is:
- `input.json` holds absolute paths. `stage-challenge.md` (`:779`) has the defender write its
  own, for that reason.
- The planned `run.json` (`tile-cost-estimator.md` §6 step 3) stores `project_root`, `input` and
  `input_manifest` as absolute paths.

This does not change identity or soundness; it only limits where a run directory can be used.

## What this is not

- **Not cargo's target-directory sharing.** Two worktrees sharing one `CARGO_TARGET_DIR` collide
  because cargo keys path crates relative to the workspace root. That is cargo's behaviour and is
  avoided with one target directory per worktree.
- **Not an absolute-path bug.** An absolute `raster` path works whenever it exists and the line
  scan finds it. §1 is about how the path is *found*, and about the silent fallback.

## Directions

Sketches with their costs. None is picked.

- **§1, finding `raster`.** Ask cargo instead of scanning text: `cargo metadata` resolves every
  dependency form (inline, table, workspace-inherited, git) to the `raster` package's manifest
  path. `Project::new` already runs `cargo metadata`, so this costs no new process if that result
  is reused. Independently, **fail instead of falling back**: report the project path and what
  was looked for. The `../../../../crates/raster` fallback cannot work, so removing it loses
  nothing. Either change can move the tile cache key for projects that were previously
  mis-fingerprinted.
- **§2, path-independent builds.** `--remap-path-prefix` for the `raster` checkout and for
  `$CARGO_HOME` in `guest_rustflags`, or risc0's Docker build (the `program-identity` goal).
  Either one changes every `program_commitment` once, so every `Raster.lock` and stored
  commitment must be regenerated.
- **§3, relocatable runs.** Store paths relative to the project root, or copy the inputs into
  the run directory. The cost is disk space, or a rule about what "relative" means for inputs
  outside the project.
