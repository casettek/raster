//! Tests of `checks::columns`: consuming a value from the storage check folds
//! exactly the digest the storage side's chain folds for it
//! (`raster_core::fraud_window`), in order, and nothing else does.

use super::*;

use crate::checks::columns::{consume_selections, consume_store_before};
use raster_core::fraud_window::{storage_state_digest, WindowChains};
use raster_core::storage_check::{selection_witnesses_commitment, StorageState};

fn state(seed: u8) -> StorageState {
    StorageState {
        position: u64::from(seed),
        root: sha(&[seed, 1]),
        index_root: sha(&[seed, 2]),
    }
}

/// What the guest folds while consuming a window equals what the storage part
/// folds producing it.
#[test]
fn consuming_folds_what_the_storage_side_folds() {
    let window = [9u8; 32];
    let (_, _, payload, _) = selecting_input_witness("arg", CfsCoordinates(vec![1]));
    let maps: Vec<BTreeMap<String, SelectionWitness>> = vec![
        BTreeMap::new(),
        [("arg".to_string(), payload)].into_iter().collect(),
    ];
    let states = [state(1), state(2)];
    let records = [sha(b"step 0"), sha(b"step 1")];

    let mut consumed = WindowChains::new(&window);
    for ((record, before), map) in records.iter().zip(&states).zip(&maps) {
        let commitment = selection_witnesses_commitment(map);
        let store = consume_store_before(&mut consumed, before);
        let selections = consume_selections(&mut consumed, map, &commitment);
        assert_eq!(store.get(), before, "the consumed value is the value");
        assert_eq!(selections.get().len(), map.len());
        consumed.records.fold(record);
    }

    let commitments: Vec<_> = maps.iter().map(selection_witnesses_commitment).collect();
    let produced = WindowChains::fold_all(
        &window,
        records
            .iter()
            .zip(&states)
            .zip(&commitments)
            .map(|((r, s), c)| (r.as_slice(), s, c)),
    );
    assert_eq!(consumed, produced);
}

/// A different store before any step, or a different selection map, moves
/// the chain: the host's comparison with the storage guest catches it.
#[test]
fn a_different_consumed_value_moves_its_chain() {
    let window = [9u8; 32];
    let fold_store = |before: &StorageState| {
        let mut chains = WindowChains::new(&window);
        let _ = consume_store_before(&mut chains, before);
        chains.store_before.value()
    };
    let mut other = state(1);
    other.position += 1;
    assert_ne!(fold_store(&state(1)), fold_store(&other));
    assert_eq!(
        fold_store(&state(1)),
        {
            let mut chain = raster_core::fraud_window::Chain::column(
                raster_core::fraud_window::Column::StoreBefore,
                &window,
            );
            chain.fold(&storage_state_digest(&state(1)));
            chain.value()
        },
    );
}
