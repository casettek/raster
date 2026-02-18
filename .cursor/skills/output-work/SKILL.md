---
name: output-work
description: Executes approved output plan phases in this implementation repo with scoped validation. Supports trivial one-off tweaks with no required plan sync or spec touchpoint.
---

# Output Work (Raster)

Use this for execution in the `raster` codebase.

## Inputs

- Approved KB plan phase(s) (optional for minor direct-work)
- KB output path for writeback (typically `outputs/<output>/OUTPUT.md`)
- KB plans index path for lifecycle sync (typically `outputs/<output>/plans/PLANS.md`)

## One-off mode (minor direct-work)

Use this mode when the change is small, scoped, and unambiguous.

1. Derive acceptance checks directly from the user request.
2. Implement the smallest valid change.
3. Run lightweight relevant checks.
4. Do not require spec touchpoint/no-op note for trivial one-off UI/content/polish tweaks.
5. Do not require KB current-state writeback for trivial one-off tweaks.

## Completion contract

Consider work complete only when all are done:

1. Requested code scope is implemented.
2. Relevant checks are run and results captured.
3. Canonical repo specs in `docs/specs/` are updated (or explicit no-op rationale exists) for non-one-off work.
4. KB writeback notes are ready for `OUTPUT.md` and plans lifecycle sync for non-one-off work.

## Post-test tweak loop (primary path)

When user testing returns tweak requests in the same chat:

1. Treat the request as a scoped follow-up work slice.
2. Classify impact:
   - minor polish (no behavior/contract change)
   - behavior change (user-visible behavior changes)
   - contract/architecture change (API/schema/system behavior)
3. Implement only the requested tweak slice.
4. If tweak is minor polish:
   - no spec touchpoint requirement
   - no KB writeback requirement
5. If tweak changes behavior/contract/architecture:
   - enforce spec touchpoint:
     - update `docs/specs/` if behavior/contract/architecture changed, or
     - record explicit no-op rationale.
   - prepare and post KB current-state writeback.

## Workflow

1. Determine mode first:
   - one-off mode for trivial UI/content/polish slices
   - standard mode for planned or behavior-impacting work
2. Read the approved KB plan phase(s).
   - In one-off mode, read the direct user request as the work scope and acceptance source.
3. Implement only in requested scope.
4. Run focused checks:
   - Single-crate changes: `cargo check -p <crate> && cargo test -p <crate>`
   - Shared/cross-crate changes: `cargo check --workspace && cargo test --workspace`
   - Substantial Rust changes before completion: `cargo clippy --workspace --all-targets`
5. For standard mode only: treat repo docs as canonical specs; update `docs/specs/` when behavior, contracts, architecture, or operations changed.
6. For standard mode only: if no spec update is needed, record a one-line "No spec changes needed" rationale in the work notes.
7. For standard mode only: prepare concise KB writeback notes including:
   - current implemented scope/status
   - current validation/quality status
   - changed repo spec paths or no-op rationale
8. For standard mode only: post writeback to KB output context (`OUTPUT.md` + relevant plan status fields) as a current-state update, not a timeline log.
