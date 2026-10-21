//! Per-step verification checks, grouped by the invariant they enforce:
//!
//! - [`control_flow`]: the step sits at valid CFS coordinates and its inputs
//!   obey the schema's input bindings — one function, [`control_flow::verify_step`],
//!   over an explicit input.
//! - [`recur`]: recur iterations are chunked as declared and advance the
//!   recur-progress stack.
//! - [`io`]: recorded input/output/external commitments match the provided
//!   witnesses, and tile steps carry a verified replay proof.
//! - [`store`]: the storage guest's receipt proves the storage transition
//!   (reads, optional write, roots) is consistent with the recorded
//!   before/after roots; this checks the receipt and its entry against this
//!   guest's state, and hands back the store after the step.
//! - [`drafts`]: draft transitions chain correctly across tile steps.
//! - [`receipts`]: inner receipts already verified in this execution, so a
//!   segment of steps verifies each shared journal once.
//! - [`entrypoint`]: `main`'s entry-argument binding is authorized against
//!   the authorization journal, once per step and once per fraud-proof
//!   chain genesis.

pub mod control_flow;
pub mod drafts;
pub mod entrypoint;
pub mod io;
pub mod receipts;
pub mod recur;
pub mod store;
