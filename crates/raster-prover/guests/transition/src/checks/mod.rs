//! Per-step semantic checks, grouped by the invariant they enforce:
//!
//! - [`recur`]: recur iterations are chunked as declared and advance the
//!   recur-progress stack.
//! - [`io`]: recorded input/output/external commitments match the provided
//!   witnesses, and a tile step's replay journal matches its record; the
//!   check returns the replay claim FRAUD WINDOW verifies.
//! - [`drafts`]: draft transitions chain correctly across tile steps.
//! - [`columns`]: the two values these checks read from the storage part,
//!   only through `Consumed`, which folds each into its column chain.
//! - [`entrypoint`]: `main`'s entry-argument binding is authorized against
//!   the authorization journal, once per step and once at the window's
//!   opening segment; `ProgramEnd` binds the output.

pub mod columns;
pub mod drafts;
pub mod entrypoint;
pub mod io;
pub mod recur;
