//! Per-step verification checks, grouped by the invariant they enforce:
//!
//! - [`cfs`]: the step sits at valid CFS coordinates and its inputs obey the
//!   schema's input bindings.
//! - [`io`]: recorded input/output/external commitments match the provided
//!   witnesses, and tile steps carry a verified replay proof.
//! - [`store`]: the internal store transition (reads, optional write, roots)
//!   is consistent with the recorded before/after roots.
//! - [`drafts`]: draft transitions chain correctly across tile steps.

pub mod cfs;
pub mod drafts;
pub mod io;
pub mod store;
