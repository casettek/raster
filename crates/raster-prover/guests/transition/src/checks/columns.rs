//! The values the transition checks read from the storage part, made explicit
//! (`transition-split` §Shared values).
//!
//! Two values cross from storage to the checks that remain here: the store
//! before the step (recur's returned-state read, `ProgramEnd`'s output read)
//! and the step's selection witnesses (recur's item and list length, io's
//! input roots). The checks that read them take [`Consumed`], and a `Consumed`
//! can only be made here, by folding the value into its column chain. So every
//! value that crosses is in a chain FRAUD WINDOW compares with the storage
//! part's, and a new cross-read that bypasses the chain won't compile.

use std::collections::BTreeMap;

use raster_core::fraud_window::{storage_state_digest, WindowChains};
use raster_core::input::{Hash32, SelectionWitness};
use raster_core::storage_check::StorageState;

/// A value another part produced, as a check here receives it. Only this
/// module can make one, and making one folds the value into its column chain.
pub struct Consumed<T>(T);

impl<T> Consumed<T> {
    pub fn get(&self) -> &T {
        &self.0
    }

    /// For tests that drive a check on its own, with no chain to fold into.
    #[cfg(test)]
    pub fn unchecked(value: T) -> Self {
        Self(value)
    }
}

/// The step's selection witnesses, as consumed.
pub type Selections<'a> = Consumed<&'a BTreeMap<String, SelectionWitness>>;

/// The store before the step, as consumed: folded into the `StoreBefore`
/// chain.
pub fn consume_store_before(chains: &mut WindowChains, before: &StorageState) -> Consumed<StorageState> {
    chains.store_before.fold(&storage_state_digest(before));
    Consumed(before.clone())
}

/// The step's selection witnesses, as consumed: their commitment folded into
/// the `Selections` chain. `commitment` is `selection_witnesses_commitment` of
/// `witnesses`, computed once per step by the caller.
pub fn consume_selections<'a>(
    chains: &mut WindowChains,
    witnesses: &'a BTreeMap<String, SelectionWitness>,
    commitment: &Hash32,
) -> Selections<'a> {
    chains.selections.fold(commitment);
    Consumed(witnesses)
}
