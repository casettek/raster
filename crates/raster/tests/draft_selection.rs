use raster::into_auth_value;
use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Account {
    txs: Vec<String>,
    balance: u64,
}

#[tile(kind = iter)]
fn set_balance(balance: u64, draft: Draft<Account>) -> Draft<Account> {
    let mut draft = draft;
    draft.balance().set(balance);
    draft
}

#[tile(kind = iter)]
fn push_tx(tx: String, draft: Draft<Account>) -> Draft<Account> {
    let mut draft = draft;
    draft.txs().push(tx);
    draft
}

#[sequence]
fn build_account_reference(balance: u64, first_tx: String, second_tx: String) -> InternalRef {
    let draft = new!(Account);
    let draft = call!(set_balance, balance, draft);
    let draft = call!(push_tx, first_tx, draft);
    let draft = call!(push_tx, second_tx, draft);
    finalize(draft).reference().clone()
}

fn run_build_account_reference(balance: u64, first_tx: String, second_tx: String) -> InternalRef {
    materialize_auth_return::<InternalRef, _>(__raster_sequence_auth_build_account_reference(
        balance, first_tx, second_tx,
    ))
}

#[test]
fn finalize_returns_selectable_internal_auth_ref() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_auth_ref_select");

    let draft = new!(Account);
    let draft = set_balance(7, draft);
    let draft = push_tx("tx-1".to_string(), draft);
    let draft = push_tx("tx-2".to_string(), draft);
    let account = finalize(draft);

    let balance = select!(u64, account.clone().balance);
    let first_tx = select!(String, account.clone().txs[0]);
    let second_tx = select!(String, account.txs[1]);

    assert_eq!(into_auth_value::<u64, _>(balance).unwrap().into_inner(), 7);
    assert_eq!(
        into_auth_value::<String, _>(first_tx).unwrap().into_inner(),
        "tx-1"
    );
    assert_eq!(
        into_auth_value::<String, _>(second_tx)
            .unwrap()
            .into_inner(),
        "tx-2"
    );
}

#[test]
fn finalized_internal_refs_support_select() {
    let reference = run_build_account_reference(11, "first".to_string(), "second".to_string());

    let balance = select!(u64, internal!(Account, reference.clone()).balance);
    let first_tx = select!(String, internal!(Account, reference.clone()).txs[0]);
    let second_tx = select!(String, internal!(Account, reference).txs[1]);

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

#[test]
#[should_panic(expected = "can only be written once")]
fn draft_rejects_duplicate_scalar_writes() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_duplicate_scalar");

    let mut draft = new!(Account);
    draft.balance().set(1);
    draft.balance().set(2);
}

#[test]
#[should_panic(expected = "must be written before finalize")]
fn finalize_requires_all_set_once_fields() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_missing_balance");

    let draft = new!(Account);
    let draft = push_tx("tx-only".to_string(), draft);
    let _ = finalize(draft);
}

#[test]
fn serialized_draft_handles_cannot_be_deserialized() {
    let _guard = raster::__private::SequenceScopeGuard::enter("draft_serde_roundtrip");

    let draft = new!(Account);
    let bytes = raster::core::postcard::to_allocvec(&draft).expect("draft marker should serialize");

    assert!(raster::core::postcard::from_bytes::<Draft<Account>>(&bytes).is_err());
}
