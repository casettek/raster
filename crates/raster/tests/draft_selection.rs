use raster::into_auth_value;
use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Account {
    txs: List<String>,
    balance: u64,
}

/// Drafts the account in this tile; returning it stores it at this tile's
/// coordinate.
#[tile(kind = iter)]
fn build_account(balance: u64, first_tx: String, second_tx: String) -> Draft<Account> {
    let mut account = Draft::<Account>::new();
    account.balance().set(balance);
    account.txs().push(first_tx);
    account.txs().push(second_tx);
    account
}

#[tile(kind = iter)]
fn set_balance_twice() -> Draft<Account> {
    let mut account = Draft::<Account>::new();
    account.balance().set(1);
    account.balance().set(2);
    account
}

#[tile(kind = iter)]
fn push_without_balance(tx: String) -> Draft<Account> {
    let mut account = Draft::<Account>::new();
    account.txs().push(tx);
    account
}

/// Writes one transaction under a 200-byte budget: a small one fits, a large
/// one exceeds it.
#[tile(kind = iter)]
fn push_under_small_budget(tx: String) -> Draft<Account> {
    let mut account = Draft::<Account>::new().__raster_with_budget(200);
    account.balance().set(1);
    account.txs().push(tx);
    account
}

#[tile(kind = iter)]
fn echo_label(label: String) -> String {
    label
}

#[sequence]
fn build_account_reference(balance: u64, first_tx: String, second_tx: String) -> StorageRef {
    let account = call!(build_account, balance, first_tx, second_tx);
    account.reference().clone()
}

#[sequence]
fn tile_after_a_creating_tile(label: String) -> StorageRef {
    let _account = call!(build_account, 7, "a".to_string(), "b".to_string());
    let echoed = call!(echo_label, label);
    echoed.reference().clone()
}

fn run_build_account_reference(balance: u64, first_tx: String, second_tx: String) -> StorageRef {
    materialize_auth_return::<StorageRef, _>(__raster_sequence_auth_build_account_reference(
        balance, first_tx, second_tx,
    ))
}

#[test]
fn a_tile_built_draft_is_a_selectable_object() {
    let reference = run_build_account_reference(11, "first".to_string(), "second".to_string());

    let balance = select!(u64, storage!(Account, reference.clone()).balance);
    let first_tx = select!(String, storage!(Account, reference.clone()).txs[0]);
    let second_tx = select!(String, storage!(Account, reference).txs[1]);

    assert_eq!(into_auth_value::<u64, _>(balance).unwrap().into_inner(), 11);
    assert_eq!(
        into_auth_value::<String, _>(first_tx).unwrap().into_inner(),
        "first"
    );
    assert_eq!(
        into_auth_value::<String, _>(second_tx)
            .unwrap()
            .into_inner(),
        "second"
    );
}

/// The draft is completed at its creating tile's close and stored at that
/// tile's own coordinate — never at a synthetic one — and the next tile keeps
/// its position.
#[test]
fn a_creating_tile_writes_at_its_own_coordinate() {
    let account = run_build_account_reference(1, "a".to_string(), "b".to_string());
    assert_eq!(account.coordinates, raster::core::cfs::CfsCoordinates(vec![1]));

    let echoed = materialize_auth_return::<StorageRef, _>(
        __raster_sequence_auth_tile_after_a_creating_tile("after".to_string()),
    );
    assert_eq!(echoed.coordinates, raster::core::cfs::CfsCoordinates(vec![2]));
}

#[test]
#[should_panic(expected = "can only be written once")]
fn draft_rejects_duplicate_scalar_writes() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_duplicate_scalar");
    let _ = set_balance_twice();
}

#[test]
#[should_panic(expected = "must be written before")]
fn completion_requires_all_set_once_fields() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_missing_balance");
    let _ = push_without_balance("tx-only".to_string());
}

/// A draft lives inside a tile: a sequence body cannot open one.
#[test]
#[should_panic(expected = "can only be called inside a tile")]
fn a_draft_cannot_be_created_outside_a_tile() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_outside_tile");
    let _ = Draft::<Account>::new();
}

#[test]
fn serialized_draft_handles_cannot_be_deserialized() {
    let draft = Draft::<Account>::from_site([0u8; 32], [0u8; 32]);
    let bytes = raster::core::postcard::to_allocvec(&draft).expect("draft marker should serialize");

    assert!(raster::core::postcard::from_bytes::<Draft<Account>>(&bytes).is_err());
}

/// The draft budget bounds what one tile run writes: payload bytes plus a fixed
/// per-op charge (`DRAFT_OP_CHARGE`).
#[test]
fn a_write_within_the_budget_passes() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_within_budget");
    let account = push_under_small_budget("small".to_string());
    assert_eq!(account.txs.as_slice(), ["small".to_string()]);
}

#[test]
#[should_panic(expected = "Draft budget exceeded")]
fn a_write_past_the_budget_is_refused() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_past_budget");
    let _ = push_under_small_budget("x".repeat(400));
}
