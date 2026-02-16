---
name: output-review
description: Reviews implemented raster changes for correctness and regressions, then verifies fixes. Use when the user asks to review work done from an output plan.
---

# Output Review (Raster)

Use this for implementation review in `raster`.

Use this for comprehensive reassessment, not routine post-test tweak execution.

## Workflow

1. Compare implementation against planned acceptance criteria.
2. Surface findings by severity (bugs/regressions first).
3. Fix issues when asked; otherwise provide an actionable fix list.
4. Re-run relevant checks after fixes.
5. Confirm spec touchpoint quality:
   - `docs/specs/` updated when required, or
   - explicit no-op rationale recorded.
6. Record review summary and residual risks for KB compounding.
