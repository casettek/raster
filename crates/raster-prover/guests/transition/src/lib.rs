//! The TRANSITION part's checks: io, recur, drafts and entrypoint over a
//! segment of steps (`part`), verifying no receipt. The guest binary
//! (`main.rs`) runs them and commits a `PartSegmentJournal`, which FRAUD
//! WINDOW stitches and compares with the other parts'
//! (`docs/proposals/transition-split.md`).

pub mod checks;
pub mod part;

// Shared with the other guests; imported at the crate root so the
// `crate::frame` and `crate::merkle_tree` paths read as they always have.
pub use guest_common::{frame, merkle_tree};

#[cfg(test)]
mod tests;
