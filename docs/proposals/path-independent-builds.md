# Proposal: `path-independent-builds` — guest image ids that do not depend on where things live

Status: **implemented** 2026-11-03 on `feature/path-fixes` (PR 1–5, §6), after the mechanism was
measured (§3). Still open: §9's questions, iris re-locking, and the checks §6 lists as not run.
Citations are against `main` at `7cfc909`. Files that exist only on `codex/windows-native` (`commands/audit.rs`,
`utils/native_build.rs`) are marked as such.

Related:
- [`build-path-dependence`](../issues/build-path-dependence.md) — the open issue. Its §1 (finding
  `raster`) and §2 (ids depend on the checkout path and `$CARGO_HOME`) are fixed here. Its §3 (run
  artifacts) is listed but deferred.
- [`program-identity.md`](./program-identity.md) — calls local builds "dev-grade (machine-local
  ids)" (`:377`), and sets "two docker-mode builds from different checkout paths produce equal tile
  image ids" as a verification goal (`:466`). This proposal meets that goal for local builds,
  without Docker.
- [`stage-challenge.md`](./stage-challenge.md) open question 6 — a defender must build at the
  claimer's checkout path.
- [`settlement-layer.md`](./settlement-layer.md) `:268-271`, `:380` — lists path-independent
  identity as a prerequisite.

## 1. Problems

Every path-related defect found so far. "New" means it is not in the issue.

| # | Problem | Where | Effect | Source |
|---|---|---|---|---|
| P1 | `raster` is found by scanning the project's `Cargo.toml` as text. Workspace inheritance, git and registry dependencies are missed. The fallback `../../../../crates/raster` is resolved from a temp dir, so it is always `/crates/raster`. The path is canonicalized, so a symlinked layout fails with a cargo "package collision". | `raster-backend-risc0/src/guest_builder.rs:106-169`, `:115`, `:258` | the guest build fails | issue §1 |
| P2 | The tile cache key hashes `raster`, `raster-core`, `raster-macros` and the user crate only. It misses any other local path dependency, such as iris's `det-num`. When P1 finds nothing, it drops the raster sources too. | `guest_builder.rs:342-375` | a stale ELF is served | issue §1, plus `det-num` (new) |
| P3 | Tile ELFs embed the absolute paths of their path dependencies and of `$CARGO_HOME` (panic locations). There is no `--remap-path-prefix`. | `guest_rustflags`, `guest_builder.rs:27` | id depends on checkout path, user name, `$CARGO_HOME` | issue §2 |
| P4 | cargo's `-C metadata` hash for a path dependency outside the building workspace includes its **absolute path**. rustc uses that hash to identify crates and order items, so a different checkout path reorders the code. Remapping paths does not change it. | cargo | id depends on checkout path **even with P3 fixed** | new, measured §3 |
| P5 | The protocol guests (`transition`, `authorization`, `chain_fraud`, `defence`) are built by `risc0_build::embed_methods()` and have P3 and P4. Verifiers check receipts against their own locally built ids (`defence.rs:58`, `chain_fraud.rs:64`; `transition.rs:230`, `:760`). | `raster-prover/build.rs` | a proof made from one checkout path should fail to verify at another (inferred from the ELF contents, not reproduced) | new |
| P6 | The generated tile guest has no `Cargo.lock`, so dependency versions are resolved at build time. Tiles carry `anyhow-1.0.104`; the transition guest, which has a lockfile, carries `1.0.102`. | `generate_guest_cargo_toml`, `guest_builder.rs:231` | id depends on when it was built and on the registry cache | new |
| P7 | The guest rustc is the lexically newest directory in `~/.risc0/toolchains`. `Raster.lock`'s `toolchain.risc0` records the CLI's `risc0-zkvm` crate version, not that rustc. | `find_risc0_cargo`, `guest_builder.rs:377-417`; `raster-cli/src/program.rs:229-237` | a toolchain mismatch moves ids without being detected | new |
| P8 | The chain runner canonicalizes the stage base directory; direct commands use the path as given. Through a symlink, the same stage gets different ids depending on how it was built. | `raster-cli/src/chain.rs:2823` | ids depend on the entry point | new, minor (goes away with P4's fix) |
| P9 | Lock drift is only detected at prove time (`program identity drift`); `run --commit` does not check. | `raster-cli/src/program.rs:336` | late, confusing failure | issue §2 |
| P10 | Run artifacts store absolute paths (`run.json` `project_root`, file inputs). | `raster-cli/src/run_manifest.rs:25` | a run directory cannot be moved | issue §3 — **out of scope** |
| P11 | **Fixed in PR 4** (no-op build 3.3 s → 0.32 s). Not path-related, found along the way, and *revised 2026-11-03* after measuring. risc0-build registers its own `methods.rs` as a rebuild trigger and rewrites that file on every run. So `raster-prover`'s build script reruns on every build, and `raster-prover` recompiles every time. No guest or `raster-core` edit can be missed: the hand-written `defence` gap on `codex/windows-native` is harmless. The real cost is the opposite: a no-op `cargo build -p raster-cli` takes 3.3 s. That may be part of the "cargo no-op build check 8.4 s" recorded for `prompt-prepare`. | `risc0-build-3.0.6/src/lib.rs:820` | every build recompiles `raster-prover` and its dependents | new, measured |
| P12 | raster's `Cargo.lock` is gitignored and was never committed (iris commits its own). Seeding a tile guest from the project's lockfile (P6) pins versions only where that lockfile is shared. For raster's own examples it is per-machine, so their committed `Raster.lock` ids can still differ between machines. | `.gitignore:5` | example ids depend on each machine's resolve | new, 2026-11-03 |

## 2. What a guest image id depends on today

An image id is a hash of the guest's loaded memory, meaning the ELF's `PT_LOAD` segments. Symbol
tables and debug info are not loaded and do not count. Two things from the local environment reach
those segments:

1. **Path strings (P3).** Every `unwrap`, `expect`, slice index and `panic!` stores its source
   location in `.rodata`. cargo gives rustc a relative path for crates under the building
   workspace's root and an absolute path for every other crate. The tile guest is its own workspace
   in a temp dir, so everything except its `main.rs` is absolute. The iris `mac_weight_page` tile
   built on 2026-10-19 contains `/home/work19/dev/src/raster-project/raster-inference/casettek/iris/raster-stages/prefill-range/src/lib.rs`,
   two paths through the old `casettek/raster` symlink, and five `/home/work19/.cargo/registry/…` paths.
2. **Code layout (P4).** cargo computes each crate's `-C metadata` from its package id. For a path
   source, it hashes the path relative to the workspace root when the path is under that root, and
   the absolute path otherwise. rustc derives each crate's stable id from that hash, and the id
   changes how items are ordered. A different checkout path therefore reorders functions, and every
   call offset changes. §3 measures this.

Paths that are the same on every machine and need no fix:
- `/root/.cargo/…/rustc-demangle` and `/home/remi/.cargo/…/no_std_strings`, which were baked in when
  risc0 built its toolchain and kernel;
- `/rustc/<commit>/…` and `library/…`, from the standard library.

## 3. Measured, 2026-11-03

**Setup.**
- **Tile:** the `advance_line_cursor` guest of hello-tiles, built with `GuestBuilder`'s exact
  template and rustflags and risc0's rustc `v1.94.1`. The lockfile is seeded from the checkout.
- **Sources:** two copies, at `~/.cache/raster-pathtest/a/raster` and
  `~/.cache/raster-pathtest/bb/much/deeper/raster`.
- **Second `$CARGO_HOME`:** a hardlinked copy of `~/.cargo/registry`.
- **Comparison:** the `PT_LOAD` segments, which are what the id hashes. Whole-file hashes differ
  anyway because of symbol tables.
- **Cost:** about 27 s per build.

The scripts are in `~/.cache/raster-pathtest-scripts/` (`run.sh`, `run2.sh`, `segments.py`), outside
the repository.

| Variant pair | What differs between them | Loaded segments |
|---|---|---|
| plain A / plain B | checkout path, `$CARGO_HOME` | **differ**: `.rodata` is 21 216 vs 21 432 bytes (path strings) |
| remap A / remap A′ | only the guest's temp directory | identical |
| remap A / remap A, second `$CARGO_HOME` | only `$CARGO_HOME` | identical |
| remap A / remap B | checkout path | **differ**: same sizes, 26 089 bytes of `.text` (call offsets) and 88 bytes of `.rodata` jump tables. Function order changed (P4) |
| staged A / staged B | checkout path, `$CARGO_HOME` | **identical** |
| staged A / staged, where the stage holds only a symlink to B | checkout path, `$CARGO_HOME`, symlinked stage | **identical** |

"remap" adds `--remap-path-prefix=<CARGO_HOME>=/cargo --remap-path-prefix=<checkout>=/raster`.
"staged" puts the guest's workspace root at a common ancestor of every path dependency and names
them by relative path. It also adds only the `$CARGO_HOME` remap (§4.3).

**What this shows.**
- **Remapping on its own is not enough.** It removes every local path string and fully handles
  `$CARGO_HOME`, but the checkout path still changes the code layout (P4).
- **The guest's temp directory never matters.**
- **Staging fixes both mechanisms.** cargo passes relative paths to rustc, and it hashes path
  sources relative to the workspace root. The only strings left in the staged ELF are
  `raster/crates/raster-core/src/tree.rs` (relative), `/cargo/registry/…`, risc0's
  `/root/.cargo/…`, and `library/…`.
- **The staged trees must be excluded from the guest workspace.** Without
  `[workspace] exclude = [...]`, cargo assigns hello-tiles to the guest workspace and
  `serde = { workspace = true }` fails with "`workspace.dependencies` was not defined". This was
  observed.
- **cargo's own fix is not usable yet.** cargo's `trim-paths` profile option would do all of this,
  but it is still unstable: both rustup's cargo 1.91.1 and risc0's cargo 1.94.1 reject it with
  "feature `trim-paths` is required".

## 4. Design

### 4.1 One build recipe for tile and protocol guests

Add a small crate, `raster-guest-build`, that turns
(path packages, guest dependencies, lockfile) into a `ProgramBinary` and an image id.

- `raster-backend-risc0` uses it to build tile guests at run time.
- `raster-prover`'s `build.rs` uses it to build the protocol guests at build time, replacing
  `risc0_build::embed_methods()`.

Today `guest_rustflags` copies risc0-build's private `encode_rust_flags` by hand
(`guest_builder.rs:24-26`). With one recipe there is one copy, and the two guest kinds cannot drift
apart.

### 4.2 Ask cargo for the path packages (P1, P2)

Run `cargo metadata --format-version 1` with the full dependency resolve; this takes 0.24 s on iris
`prefill-range`. From the user crate, walk the normal dependencies. Every package with
`source: null` is a path package. For iris `prefill-range` that is 11 packages, including
`det-num`. Use the paths as cargo reports them and **never canonicalize**.

- `raster` is the package named `raster` in that graph. If it is missing, report the project and
  say that no `raster` package was found in its dependency graph. There is **no fallback**.
- The cache key hashes `Cargo.toml` and `src/` of every path package in the guest's graph,
  replacing the fixed list of three crates.

### 4.3 Staged build (P3, P4, P6, P8)

*Revised 2026-11-03.* The first draft put A at the raster checkout for in-repo examples. That
cannot work: the stage root's `Cargo.toml` would replace raster's root manifest, and with it the
`[workspace.dependencies]` that `workspace = true` inherits from. The rule below keeps every
workspace root inside a stage entry.

1. **Find the workspace roots.** Let **W** be the workspace roots of the path packages: for each
   one, the nearest ancestor `Cargo.toml` with a `[workspace]` table that does not exclude it, or
   the package directory itself when there is none.
2. **Choose A.** Let **A** be the common ancestor of W, compared as plain path strings. If A is
   itself in W, move it up one level. A must sit strictly above every workspace root.
3. **Build the stage.** Create a fresh temp directory as the **stage**. For each top-level entry
   under A that contains an element of W, link `stage/<name>` to `A/<entry>`: a symlink on Unix, a
   copy without `target/` elsewhere. Entries keep their real names, except one case. When the
   raster checkout is the **only** entry (an in-repo example, a protocol guest), it is staged as
   `raster`, so the checkout's own name never reaches an id. *Revised 2026-11-04 after review:*
   the first version always renamed it, which broke a sibling project depending on
   `../raster-main/crates/raster`. Inside the stage, that path named an entry that did not exist,
   and `compute_fingerprint` failed before any build. A sibling's manifests name the checkout, so
   the name is part of its source anyway.
4. **Write the stage root `Cargo.toml`.** It has `[workspace] exclude = [<names>]`, and:
   - for a **tile guest**, it is also the generated guest package, with dependencies by relative
     path, such as `raster = { path = "raster/crates/raster" }`. The guest's `raster` path is taken
     from a probe resolve: the user crate's own manifests, resolved inside the stage. That gives
     the stage path in the layouts above. When the project names raster by an **absolute** path it
     gives that absolute path, so cargo never sees a second `raster` package (that case had also
     regressed to a "package collision"). Those ids then depend on raster's location, and the
     build warns;
   - for a **protocol guest**, it has no package and sets
     `members = ["raster/crates/raster-prover/guests/<g>"]` (§4.4).
5. **Seed the lockfile.** Copy the right `Cargo.lock` to `stage/Cargo.lock` (P6): the project
   workspace's lock for a tile, or the guest's own committed lock for a protocol guest. A tile
   then uses the same dependency versions as the project's native build.
6. **Remap `$CARGO_HOME`.** Add `--remap-path-prefix=<CARGO_HOME>=/cargo` to the rustflags. This
   covers registry and git dependencies.

| Guest | W | A | Stage entries | Path strings in the ELF |
|---|---|---|---|---|
| `examples/hello-tiles` tile | the checkout | its parent | `raster` | `raster/crates/raster-core/…`, `raster/examples/hello-tiles/…` |
| iris `prefill-range` tile (sibling layout) | the raster checkout, iris's root | `raster-project/` | `raster`, `iris` | `raster/crates/…`, `iris/raster-stages/…` |
| protocol guest | the checkout | its parent | `raster` | `raster/crates/raster-core/…`, `raster/crates/raster-prover/guests/…` |

Once this lands:
- **An image id no longer depends on:** the absolute location, the checkout's directory name, the
  user name, `$CARGO_HOME`, the temp directory, or whether the layout is reached through a symlink
  (P8).
- **It still depends on:**
  - the relative layout beneath A;
  - in a sibling layout, the real entry names, such as `iris` and the raster checkout's name
    (open question 1), which the project's manifests spell out anyway;
  - the toolchain (§4.5);
  - the lockfile.

A defender who builds the examples from `/tmp/defender-raster` gets the claimer's ids.

**Measured.** §3's staged rows used exactly this layout for hello-tiles (A one level above the
checkout, entry `raster`). On 2026-11-03 a minimal two-crate model of the protocol-guest layout
(an excluded guest without its own `[workspace]`, staged as a member under a renamed `raster`
entry) gave identical `-C metadata` hashes for both crates. The two runs used checkout
`a/raster` with stage `s1`, and checkout `x/y/defender-raster` with stage `deep/er/stage2`. In
both, rustc received relative source paths.

### 4.4 Protocol guests (P5, P11)

*Revised 2026-11-03, after the PR 4 spike.* The first design made each guest a member of the stage
workspace. That needs the guest to drop its own `[workspace]`, and then cargo assigns it to the
repository workspace and refuses ("believes it's in a workspace when it's not"). A root `exclude`
does not help: cargo ignores it for a package nested inside another member's directory, and the
guests sit inside `crates/raster-prover/`. A two-crate model without the intermediate package
had passed; the real layout did not.

**Rehoming.** Each guest is rehomed at the stage root instead (`GuestCrate::Package`):
- the stage root's `Cargo.toml` is the guest's own manifest, with every `path` dependency
  rewritten to its stage path and its `[workspace]` replaced by the stage's;
- the guest's other top-level entries (`src/`) are linked beside it.

The guest's sources are then seen as `src/...`, and its place in the checkout never reaches the id.
The guest manifests stay unchanged and keep their own `[workspace]`, so each guest still builds
standalone. The root `exclude` added in PR 1 is no longer needed and is reverted. `stage_plan`
finds the raster checkout through `raster-core` as well as `raster`, since protocol guests link
only `raster-core`. Each guest's committed `Cargo.lock` seeds its stage, and the resolved lock is
written back only if it changed.

**Build script.** `build.rs` builds the guests through `raster-guest-build` instead of
`risc0_build::embed_methods()`. It writes the same `methods.rs` constants, `<NAME>_ELF`,
`<NAME>_PATH` and `<NAME>_ID`, with ids from `risc0_binfmt::compute_image_id`, so no build-time
`risc0-zkvm` is needed. It keeps `RISC0_SKIP_BUILD`'s empty ELF and zero id, which
`commands/audit.rs:72` on `codex/windows-native` relies on. It also fixes P11:
- `methods.rs` and the packaged `.bin` files are written only when their content changes;
- the rerun triggers are the guests' path packages' `Cargo.toml`, `src/` and `build.rs`, the
  guests' lockfiles, and `RISC0_SKIP_BUILD`, `RISC0_RUST_TOOLCHAIN_PATH` and `CARGO_HOME`.

risc0-build cannot do this itself:
- it removes every `CARGO*` env var before running cargo;
- it accepts extra rustc flags only as a static list in `[package.metadata.risc0]`, and a
  machine-specific remap cannot be static;
- it cannot stage a build.

### 4.5 Toolchain (P7)

- **Selection:** pick the risc0 Rust toolchain the way risc0-build does (rzup's default version),
  not the lexically newest directory.
- **Recording:** record its version, such as `v1.94.1-rust`, in `Raster.lock` as
  `toolchain.rustc` (lock `format` 2).
- **Checking:** a mismatch between the lock and the installed toolchain becomes a warning at build
  time and an error at prove time. The error names the toolchain instead of saying only
  `program identity drift`.

### 4.6 Not in this proposal

- P10, run artifacts (issue §3).
- P9, an early drift check in `run --commit`. Open question 4 asks whether it should ride along.
- Docker builds (§8).

## 5. Migration: every id changes once

- `GUEST_BUILD_ABI` goes from 3 to 4.
- **Every** tile image id, every `program_commitment`, and all four protocol guest ids change.
- These must be regenerated in the same change:
  - the six committed locks (`examples/hello-tiles`, `examples/chain-example/{phase1-normalize,
    phase2-aggregate,phase3-report,planner,step}`);
  - iris's ten `raster-stages/*/Raster.lock`, which need re-locking anyway since the 2026-11-03
    move.
- Stored commitments, `.challenge` files and fraud proofs made before the change no longer verify.

## 6. Implementation plan

Five PRs, in order. Each one leaves `main` working. Only PR 3 and PR 4 move ids. PR 3 moves the
tile ids and regenerates the locks; PR 4 moves only the protocol ids, which no lock contains.

**Before starting.** Work from `main`. Whichever of this work and the `codex/windows-native` merge
lands second has to adapt to the other: windows-native adds `commands/audit.rs` (another
`with_user_crate` caller) and a hand-written `rerun-if-changed` list in `build.rs`. Use a separate
`CARGO_TARGET_DIR` per worktree.

### PR 1 — prep, no id change (small) — done 2026-11-03 on `feature/path-fixes`
- **Root workspace:** add `exclude = ["crates/raster-prover/guests"]` to the root `Cargo.toml`.
  The guests keep their own `[workspace]` for now. *Reverted in PR 4:* cargo ignores this
  `exclude` for packages nested in `crates/raster-prover/`, and rehoming does not need it (§4.4).
- **Dropped:** a generated `rerun-if-changed` list. On `main`, measured, the build script already
  reruns on every build (P11), so the list would change nothing. The real fix belongs in PR 4.
- **Verified:**
  - the root workspace's members are unchanged;
  - all four `*_GUEST_ID`s are unchanged after a real guest build;
  - each guest still resolves standalone (`cargo metadata` in its directory);
  - `cargo check --workspace --all-targets` passes.

### PR 2 — `raster-guest-build` crate, library only, no caller yet (medium) — done 2026-11-03 on `feature/path-fixes`

Modules:
- **`packages`:**
  - `PathPackages::resolve(manifest, root)`: `cargo metadata` with the full resolve (the crate
    already uses `cargo_metadata` 0.19), walking normal dependencies from `root`.
  - Returns each path package's name, directory as cargo reports it, and workspace root (§4.3
    step 1).
  - A missing `raster` is an error naming the project.
- **`stage`:**
  - computes A, the entries and the `raster` rename;
  - `Stage::create()`: a temp dir plus links (symlink on Unix, copy elsewhere);
  - `Stage::relative(dir)`.
- **`recipe`:**
  - the guest rustflags, moved from `guest_rustflags`, plus the `$CARGO_HOME` remap
    (`$CARGO_HOME`, else `$HOME/.cargo`);
  - toolchain selection through `rzup` (already in the lockfile via risc0-build), returning the
    toolchain id;
  - `RECIPE_VERSION = 4`.
- **`build`:**
  - takes `GuestSpec::Tile { deps, main_rs }` or `GuestSpec::Member { path }`, plus a lockfile;
  - writes the stage manifest, runs cargo, packages the `ProgramBinary` with the v1compat kernel,
    and returns the binary and its image id (`risc0_binfmt::compute_image_id`).
- **`fingerprint`:** hashes `Cargo.toml` and `src/` of every path package, the lockfile, the
  toolchain id and `RECIPE_VERSION`.

Tests, needing no toolchain:
- A and the entries for the hello-tiles, sibling-iris, protocol-guest and single-package layouts;
- the `raster` rename;
- the exclude list;
- a missing `raster` gives the error;
- the `PathPackages` walk on this repository's hello-tiles;
- the fingerprint moves on an edit to an extra local crate (the `det-num` case) and does not move
  on `src/main.rs`.

Ignored test, needing the risc0 toolchain: §3's staged experiment automated. One hello-tiles
tile, two checkout copies at different paths with different `$CARGO_HOME`s, equal image id.

**Outcome.**
- **Unit tests:** 20 pass. They cover the stage layouts and the `raster` rename, workspace-root
  discovery (`exclude`, explicit members, `package.workspace`), the real hello-tiles resolve, the
  missing-`raster` error, and the generated and member manifests. They check that the fingerprint
  tracks a `det-num`-style extra crate, ignores `main.rs` and `src/bin`, and is equal for the same
  sources at two paths. They also check that a dropped stage removes its links and not the
  checkout.
- **Integration test:** `tests/two_checkouts.rs` (`--ignored`, 57 s) builds the
  `advance_line_cursor` tile from copies at `a/raster` and `b/much/deeper/defender-raster`. Both
  give image id `e9ce4c3d…`; neither ELF contains its checkout path or `$CARGO_HOME`; the byte and
  word encodings of the id agree. It uses the developer's `$CARGO_HOME` for both copies; §3
  measured the `$CARGO_HOME` case.
- **Differences from the plan:**
  - `rzup` is a dependency with `default-features = false`, as risc0-build has it. Its defaults
    pull in the AWS SDK, which needs rustc 1.94.
  - The C compiler is set as `CC_riscv32im_risc0_zkvm_elf`, not the global `CC` risc0-build sets,
    so host build scripts keep the host compiler.
  - The build removes every `CARGO*` variable and `RUSTUP_TOOLCHAIN`, then sets `CARGO_HOME`
    explicitly, so the remap matches the path cargo uses even inside a build script.
  - Packaging (`ProgramBinary` plus `risc0_binfmt::compute_image_id`, which `risc0_zkvm`
    re-exports) is checked only by the integration test, because a unit test has no real user ELF.

### PR 3 — tile guests switch over; tile ids change (medium) — implemented 2026-11-03 on `feature/path-fixes`
- **`GuestBuilder`:** becomes a thin adapter over `raster-guest-build`. Delete
  `extract_raster_path`, the `../../../../crates/raster` fallback and the `canonicalize`.
  `with_user_crate` stores the path, and resolution errors surface from `compile_tile`. The
  callers stay unchanged: they all pass the project root (`commands.rs:99,192,213`,
  `commands/prove.rs:80`, `commands/replay.rs:109`, `chain.rs:1848`; plus `commands/audit.rs` on
  windows-native).
- **`Risc0Backend::compile_tile`:** builds in a `Stage` instead of `tempfile::tempdir()`. The
  cache key becomes the tile source hash plus the new fingerprint.
- **Lockfile:** the project workspace's `Cargo.lock` (`workspace_root` from metadata). If it is
  missing, warn that ids will not be reproducible, and build. For raster's own examples this
  needs P12 settled first (open question 5).
- **Locks:** regenerate the six committed locks with
  `target/debug/cargo-raster raster build --backend risc0`.
- **Tests:**
  - the existing CLI suites (`chain_repeat_cli`, `recur_cli`, `replay_profile_cli`) and the
    ignored `stage_challenge_cli` in dev mode;
  - new: issue §1's workspace-inheritance reproducer builds;
  - new: a symlinked sibling layout builds (the old `casettek` case).
- **Accept:** the PR 2 ignored test passes. `stage-challenge` "anyone defends" succeeds from a
  second worktree at another path; today it fails with `program identity drift`.

**Outcome (PR 3).**
- **The builder.** `GuestBuilder` is an adapter over `raster-guest-build`. `Risc0Backend` resolves
  it once, with a `OnceLock`, instead of rebuilding it for every tile.
- **One guest target directory per project** (`target/raster/guest-target`; tiles compile one at a
  time, `program.rs:174`). hello-tiles' 24 guests now build in **90 s**, instead of a from-scratch
  build per tile (28 s each when built alone). A tile built alone with a fresh target directory
  matches its shared-directory build exactly; checked for the first and the last tile.
- **The cache key** covers the staged guest's own resolve for the guest target: the exact
  external crate versions, plus `Cargo.toml` and library sources of every path package it
  compiles, named by stage path. It replaces the fixed three-crate list, and the old docs' "not
  covered: external crate versions" no longer holds.
- **Across checkouts.** A git worktree at `/tmp/pr3-check/x/y/defender-raster` (same commit, plus
  this checkout's `Cargo.lock`) builds a hello-tiles `Raster.lock` byte-identical to this
  checkout's: `program_commitment` `8db95fff…`. The old `7a09d393…` and all 24 tile ids changed
  once, as §5 expects.
- **Locks regenerated:** all six committed locks. Backups of the old ones are outside the
  repository.
- **New unit tests** (no toolchain):
  - `raster = { workspace = true }` reached through a symlinked sibling checkout resolves (both
    issue §1 reproducers);
  - a project without `raster` gets an error naming it;
  - the fingerprint is equal at two checkout paths.
- **Dependencies.** `risc0-binfmt`, `risc0-zkos-v1compat` and `risc0-zkvm-platform` moved out of
  `raster-backend-risc0`; `tempfile` is now a dev-dependency. `cargo_metadata`, `risc0-build`,
  `bincode` and `anyhow` were already unused there, and are left for a separate cleanup.
- **Removed dead code:** `Risc0Backend::load_elf`, which read a `guest.elf` nothing writes, and
  `GuestBuilder::write_artifacts`.
- **Tests, all passing** (2026-11-03):
  - `cargo test -p raster-cli`: 92 unit tests, `chain_repeat_cli` (12), `chain_stage_cli` (7),
    `recur_cli` (11), `replay_profile_cli` (4), `show_cli` (11); 43 min;
  - ignored `replay_profile_cli` (hello-tiles replayed in the RISC Zero executor with the new
    guests);
  - ignored `stage_challenge_cli`, dev mode: 7 claim, challenge and defend scenarios, 14 min.
- **For PR 5:** spec docs name functions that are gone:
  - `docs/specs/core/2-compile/03-tile-artifact-generation-elfs.md` (`find_risc0_cargo`);
  - `04-artifact-identity-and-reproducibility.md` (`write_artifacts`, `find_risc0_cargo`,
    `build_guest`, `generate_guest_cargo_toml`);
  - `05-program-bundle-and-manifests.md`;
  - `docs/proposals/zkvm-dry-run.md:76`.

### PR 4 — protocol guests switch over; protocol ids change (medium) — implemented 2026-11-03 on `feature/path-fixes`
- **Spike:** build the real `transition` guest staged from two checkout copies and compare
  `TRANSITION_GUEST_ID`.
- **`build.rs`:** replace `risc0_build::embed_methods()` with `raster-guest-build`, keeping the
  constant names and the `RISC0_SKIP_BUILD` behaviour. Drop the `risc0-build` build-dependency.
- **Always-rebuild (P11):** see §4.4.
- **Locks:** none move. `program_commitment` covers only the manifest, the CFS and the tile ids
  (`ProgramDefinition`, `raster-core/src/program.rs:72-77`). Stored fraud proofs and receipts do
  carry protocol ids, so they stop verifying.
- **Accept:**
  - `raster-prover` built from two worktree paths gives equal `*_GUEST_ID`;
  - `verify_fraud_proof` accepts a proof made in the other worktree;
  - the GPU fraud path still proves.

**Outcome (PR 4).**
- **Spike.** The real `transition` guest, built from copies at `a/raster` and
  `b/much/deeper/defender-raster`, has the same image id (`e100f900…`, about 30 s each). Neither
  ELF contains its checkout path or `$CARGO_HOME`. The spike found the member-design failure and
  led to rehoming (§4.4).
- **Across checkouts.** This checkout's `build.rs` build gives the spike's `TRANSITION_GUEST_ID`
  (`[16318689, 920494147, …]`). A git worktree at `/tmp/pr4-check/x/defender-raster`, with this
  PR's changes applied, built `raster-prover` (113 s from clean) with **all four** `*_GUEST_ID`s
  identical to this checkout's. The four ids changed once.
- **No-op builds (P11).** The build script no longer reruns, `raster-prover` is not recompiled,
  and a no-op `cargo build -p raster-cli` takes 0.32 s instead of 3.3 s.
- **Under a rustc wrapper.** The first run under `cargo clippy` failed. clippy sets
  `RUSTC_WORKSPACE_WRAPPER=clippy-driver`; the guest build inherited it and compiled the guest
  with the host sysroot (`E0463: can't find crate for std`). Guest builds now strip
  `RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER`, which also covers sccache and tile guests.
  `cargo clippy -p raster-prover` passes.
- **Lockfiles.** The committed guest lockfiles are unchanged after the build.
- **Tests, all passing** with the new protocol ids:
  - `raster-guest-build`: 21 unit tests;
  - `raster-prover`: 61 tests (9 min);
  - `cargo test -p raster-cli`: the same counts as in PR 3;
  - ignored `stage_challenge_cli`: 7 claim, challenge and defend scenarios, in dev mode.
- **Faster suites.** The CLI suites ran far faster than in PR 3: `chain_repeat_cli` 1207 → 78 s,
  `show_cli` 641 → 4.5 s, the whole suite 43 min → 150 s, `stage_challenge_cli` 14 min → 93 s.
  These suites invoke the CLI many times, and each invocation's cargo build used to recompile
  `raster-prover` (P11), which is the likely main cause. Colder caches in the PR 3 run were not
  ruled out.
- **Cross-checkout checks** (2026-11-03, worktree at `~/.cache/xw/b/defender-raster`, commit
  `7bbb25d`, its own binary and protocol guests):
  - **Defence (PR 3's acceptance check).** The claimer ran chain-example in this checkout, and a
    false challenge against `normalize` (`--forge item`) was admitted. The worktree got only the
    public bundle (`chain-commitment`, `chain-shape`, `.challenge/{challenge,commit}.bin`, and the
    stage's input manifest plus `input.json` pointed at its own checkout), with the claimer's run
    directory hidden. It defended the challenge (refuted). Its defence verified with its own binary
    and with this checkout's, against this checkout's challenge file.
  - **`verify_fraud_proof`, dev mode.** A fraud proof made in the worktree (hello-tiles, step 1
    falsified, window 2) verifies in this checkout: `Finished`, the refuted commitment matches, and
    `program_commitment` is `8db95fff…`. A fake receipt's `verify` does compare the image id
    (`risc0-zkvm-3.0.6/src/receipt.rs:188-189`). As a negative control, the committed
    `examples/hello-tiles/fraud_commit.bin.fraud-proof`, a real proof made with the old guests,
    fails against the new id ("claim digest does not match").
  - **GPU.** At risc0's default segment size the transition guest's proof runs out of memory on the
    RTX 3070 (8 GB, about 2 GB of it held by a running game), the known limit with no env knob in
    risc0 3.0.6. With a temporary `segment_limit_po2(18)` knob, applied only in the scratch
    worktree, the real fraud proof built in 157 s (GPU memory peak 7.1 GB). The 3.8 MB composite
    receipt verifies in this checkout against its `TRANSITION_GUEST_ID`, not in dev mode.
- **Follow-ups:**
  - `examples/hello-tiles/fraud_commit.bin.fraud-proof` is a committed artifact that no longer
    verifies, since PR 4 moved the protocol ids; regenerate or remove it.
  - A segment-size setting (for example `RASTER_SEGMENT_PO2`) would let 8 GB cards prove the
    transition guest without a patch.

### PR 5 — toolchain record, and docs (small) — implemented 2026-11-03 on `feature/path-fixes`
- **`Raster.lock`:** gains `toolchain.rustc` (`#[serde(default)]`; `format` 2).
- **Checking:** build warns on a lock and toolchain mismatch; prove fails with a message that
  names both toolchains.
- **Docs:**
  - mark the issue's §1 and §2 fixed;
  - update `program-identity.md:377` and `stage-challenge.md` open question 6;
  - state in the README note that only the relative layout matters, and that the raster checkout
    may be named anything.

**Outcome (PR 5).**
- **What the lock records.** `LockToolchain.rustc` holds the guest compiler's `rustc -V` line,
  e.g. `rustc 1.94.1-dev (06e01cb0d 2026-04-09)`. That line names the compiler commit and no host
  platform. It comes from `Toolchain::rustc_version` in `raster-guest-build`, using the same
  selection the guests build with. The field is optional, so format-1 locks still parse; the lock
  `format` is now 2.
- **Build.** Before overwriting a lock that records a different compiler, `build` warns that tile
  ids may all change.
- **Prove.** On drift, the error names both compilers and tells a verifier to install the lock's
  toolchain (`rzup install rust <version>`), not to rebuild the lock. With the same or an unknown
  compiler the message is unchanged. The message is a pure function (`drift_error`), with unit
  tests.
- **Locks.** All six committed locks were regenerated: `format` 1 → 2 and the new `rustc`. Every
  image id and `program_commitment` is unchanged.
- **Docs.**
  - The issue's §1–§2 are marked fixed.
  - Updated: `program-identity.md`'s caveat, `stage-challenge.md` open question 6 and its
    §Verification note, `settlement-layer.md` §5 and its prerequisite list.
  - The spec pages `2-compile/03`, `04` and `05` and `zkvm-dry-run.md:76` no longer name removed
    functions.
  - The README note and the `cargo raster init` template no longer say a symlinked raster path
    breaks guest builds; since PR 3 it does not.
- **A flaky test, fixed.** The new `rustc_version` test executes a script it has just written.
  Under parallel tests that can fail with ETXTBSY: a sibling test's fork briefly holds the file
  open. It failed once in 21 runs; the test now retries that error. A real toolchain's `rustc` is
  never freshly written.

### Outside this repository, after PR 3 and PR 4
- **iris:** re-lock all ten `raster-stages/*/Raster.lock` (the user commits in iris).
- **iris-box:** `lockdiff` between the `local` slot and a slot at another path must say `same`.
  The `local` slot's path mirroring then stops mattering; simplify it later.

## 7. Verification

- **Tile ids across machines:** an ignored integration test (it needs the risc0 toolchain) builds
  hello-tiles from two copies of the checkout, at different paths and with different
  `$CARGO_HOME`s, and asserts the two `Raster.lock`s are equal. This automates §3's staged rows.
- **Protocol ids across machines:** `raster-prover` built from two worktree paths gives equal
  `*_GUEST_ID`s.
- **Defence from another path:** `stage-challenge.md` §Verification, "anyone defends". A defender in
  `/tmp/defender-raster` defends successfully; today it fails with `program identity drift`.
- **iris across machines:** an iris-box slot at a path different from the laptop's gives the same
  locks (`iris-box lockdiff` says `same`).
- **No local paths left:** no guest ELF contains `/home/`.
- **Issue §1:** the issue's workspace-inheritance reproducer builds.

## 8. Alternatives considered

- **`--remap-path-prefix` alone.** Measured insufficient (§3, P4).
- **risc0 Docker builds** (`embed_methods_with_options` plus `use_docker`). These remove the path
  and pin the toolchain too, but every `raster-prover` build would need Docker and would be much
  slower. They are better kept as a later "release" mode on top of this, not instead of it.
- **A `RUSTC_WRAPPER` that rewrites `-C metadata`.** It works around P4 but depends on cargo
  internals and risks collisions between same-named crates.
- **cargo `trim-paths`.** The right long-term mechanism, but unstable (§3). When it stabilizes, it
  can replace staging and the remap.
- **Redirecting path dependencies with `[patch]` in `.cargo/config.toml`.** This moves where the
  path is written, not what the ELF contains. It also falls back to crates.io lookups when the
  config is missing (tested 2026-11-02).

## 9. Open questions

1. **Names of entries other than `raster` are part of identity.** For example, a sibling `iris`
   checkout cloned as `iris-main` gets different ids. The raster checkout's own name no longer
   matters (§4.3). Should this be accepted as convention? Or should the stage also give the
   project's entry a fixed name? That is only possible when no manifest refers to it by name, and
   raster never refers to iris.
2. **Windows.** Stage by copy (the plan's default), or by directory junction? Native Windows
   builds skip guest builds today (`RISC0_SKIP_BUILD=1` in `utils/native_build.rs:45` on windows-native), so this is low
   stakes.
3. **Protocol ids.** Should the expected protocol guest ids be committed (for example
   `crates/raster-prover/guest-ids.json`) and checked by a test, so a change that moves them is
   visible in review?
4. **Drift check.** Should P9 ride along, so that `run --commit` compares the lock with the built
   ids?
5. **Commit raster's `Cargo.lock` (P12)?** **Decided 2026-11-03: yes.** The line was removed from
   `.gitignore`. The four real guests' lockfiles are committed too; the five leftover
   `guests/*/Cargo.lock` without a `Cargo.toml` are not. Recommended because `raster-cli` is a
   binary, and without a shared lock the examples' tile ids differ wherever the resolve differs.
   The alternative is a committed per-guest lockfile. That is more files to keep current, and it
   would not pin the native build.
