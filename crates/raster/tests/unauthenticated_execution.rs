//! Unauthenticated execution: same values, no storage.
//!
//! `docs/proposals/unauthenticated-execution.md`. This is its own test binary
//! because the mode is a process-lifetime decision (§1) — every test here runs
//! unauthenticated, and every *other* test binary stays authenticated, which is
//! the property the whole design rests on.
//!
//! `examples/hello-tiles` cannot serve as the equivalence fixture: it uses
//! `new!` and `call_recur!` in ten places, and drafts and recur are out of
//! scope for v1 (§7). These cases deliberately stay inside the supported
//! surface — tile boundaries, `select!`, and the sequence call binding.

use raster::prelude::*;
use raster::{AuthMode, Block, List};
use raster_runtime::auth::force_auth_mode;
use serde::{Deserialize, Serialize};
use std::sync::Once;

static MODE: Once = Once::new();

/// Pin the mode before anything reads it. Every test calls this first; the
/// `Once` makes concurrent test threads agree, and `force_auth_mode` errors
/// rather than silently disagreeing if something resolved the mode already.
fn unauthenticated() {
    MODE.call_once(|| {
        force_auth_mode(AuthMode::Unauthenticated)
            .expect("mode was already resolved before this test could pin it");
    });
    assert_eq!(raster::auth_mode(), AuthMode::Unauthenticated);
}

/// The binding a `call!` inside a `#[sequence]` produces. Called directly here
/// because a non-`main` sequence is not a native function.
fn bind<T>(value: T) -> AuthRef<T>
where
    T: Serialize + serde::de::DeserializeOwned + 'static,
{
    raster::__private::bind_infallible_call(value)
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Selectable)]
struct Address {
    city: String,
    lines: List<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Selectable)]
struct PersonalData {
    name: String,
    age: u32,
    addresses: List<Address>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct CollectiveGreeting {
    title: String,
    lines: List<String>,
}

fn sample() -> PersonalData {
    PersonalData {
        name: "John".into(),
        age: 41,
        addresses: List::from(vec![
            Address {
                city: "London".into(),
                lines: List::from(vec!["221B Baker Street".into(), "Flat B".into()]),
            },
            Address {
                city: "Madrid".into(),
                lines: List::from(vec!["Main Plaza".into(), "Floor 2".into()]),
            },
        ]),
    }
}

#[tile]
fn greet(name: String) -> String {
    format!("Hello, {name}!")
}

#[tile]
fn join_lines(lines: Block<String>) -> String {
    lines.iter().cloned().collect::<Vec<_>>().join(" | ")
}

#[tile]
fn describe(city: String, age: u32) -> String {
    format!("{city}/{age}")
}

fn inline<T>(binding: AuthRef<T>) -> T {
    match binding {
        AuthRef::Inline(value) => value,
        AuthRef::Storage(_) => panic!("expected an inline binding, got a storage binding"),
    }
}

/// A tile output binds without entering storage — the whole point of the mode.
#[test]
fn tile_outputs_bind_without_storage() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter("tile_outputs_bind_without_storage");

    let bound = bind(String::from("Hello, John!"));
    assert!(
        matches!(bound, AuthRef::Inline(_)),
        "an unauthenticated tile output must not be a storage binding"
    );
    assert_eq!(inline(bound), "Hello, John!");
}

/// A tile still accepts a binding as an argument: the read side already
/// understood the value-carrying variant, so nothing changed there (§5).
#[test]
fn a_binding_still_crosses_a_tile_boundary() {
    unauthenticated();
    let _guard =
        raster::__private::SequenceScopeGuard::enter("a_binding_still_crosses_a_tile_boundary");

    assert_eq!(greet(bind(String::from("John"))), "Hello, John!");
}

/// The case the proposal is written around: `select!` lowered to a plain field
/// access still narrows, and the result still feeds a tile.
#[test]
fn select_narrows_a_field_and_feeds_a_tile() {
    unauthenticated();
    let _guard =
        raster::__private::SequenceScopeGuard::enter("select_narrows_a_field_and_feeds_a_tile");

    let person = bind(sample());
    let name = select!(String, person.name);
    assert!(matches!(name, AuthRef::Inline(_)));
    assert_eq!(greet(name), "Hello, John!");
}

/// Nested paths, literal indices, and a whole-`List` selection.
#[test]
fn select_walks_nested_paths_and_indices() {
    unauthenticated();
    let _guard =
        raster::__private::SequenceScopeGuard::enter("select_walks_nested_paths_and_indices");

    let person = bind(sample());

    let addresses = select!(List<Address>, person.clone().addresses);
    assert_eq!(inline(addresses.clone()).len(), 2);

    let second = select!(Address, addresses[1]);
    assert_eq!(inline(select!(String, second.clone().city)), "Madrid");
    assert_eq!(inline(select!(String, second.lines[0])), "Main Plaza");
}

/// A range selection yields a `Block<T>`, the shape a tile can take whole.
#[test]
fn select_range_yields_a_block() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter("select_range_yields_a_block");

    let person = bind(sample());
    let address = select!(Address, person.addresses[0]);
    let lines = select!(Block<String>, address.lines[0..2]);

    assert_eq!(join_lines(lines), "221B Baker Street | Flat B");
}

/// Two selections into one tile, including a non-`String` scalar.
#[test]
fn multiple_selections_reach_one_tile() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter("multiple_selections_reach_one_tile");

    let person = bind(sample());
    let age = select!(u32, person.clone().age);
    let address = select!(Address, person.addresses[0]);
    let city = select!(String, address.city);

    assert_eq!(describe(city, age), "London/41");
}

/// Values chain through several bindings, as they would along a sequence.
#[test]
fn bindings_chain_through_several_tiles() {
    unauthenticated();
    let _guard =
        raster::__private::SequenceScopeGuard::enter("bindings_chain_through_several_tiles");

    let person = bind(sample());
    let name = select!(String, person.name);
    let greeted = bind(greet(name));
    let again = bind(greet(greeted));

    assert_eq!(inline(again), "Hello, Hello, John!!");
}

/// **`select!` dispatches on the base's provenance, not on the mode.** A
/// storage-backed base keeps composing a selector even with authentication off,
/// so a large source is read by indexed lookup instead of being materialized at
/// every step. This is what lets `call_recur!` keep its raster-indexed source in
/// this mode, and what stops the mode being unusable at scale.
#[test]
fn a_storage_backed_base_stays_storage_backed() {
    unauthenticated();
    let _guard =
        raster::__private::SequenceScopeGuard::enter("a_storage_backed_base_stays_storage_backed");

    let reference = raster::store_value(&sample()).expect("store personal data");
    let stored = raster::typed_storage::<PersonalData>(reference);

    let addresses = select!(List<Address>, storage!(PersonalData, stored.reference().clone()).addresses);
    assert!(
        matches!(addresses, AuthRef::Storage(_)),
        "a storage-backed base must not be materialized just because auth is off"
    );

    // And narrowing further stays storage-backed rather than resolving the
    // parent at each step.
    let city = select!(String, addresses[0].city);
    assert!(matches!(city, AuthRef::Storage(_)));
}

/// **A storage-backed base indexed by a tile-produced value** — the combination
/// §5.3 and §5.4 each half-covered and neither owned. Because the base stays
/// storage-backed, the index reaches `push_bound_index` (selector lowering)
/// rather than the accessor arm, and an inline index there used to be rejected
/// as unauthorized. In this mode there is no lineage to protect and no verifier
/// to satisfy, so the segment degrades to a plain index and picks the element
/// the authenticated run would pick.
#[test]
fn a_storage_backed_base_accepts_a_tile_produced_index() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter(
        "a_storage_backed_base_accepts_a_tile_produced_index",
    );

    let reference = raster::store_value(&sample()).expect("store personal data");
    let stored = raster::typed_storage::<PersonalData>(reference);
    let addresses =
        select!(List<Address>, storage!(PersonalData, stored.reference().clone()).addresses);

    // A tile output: inline, with nothing committing to it. This is what the
    // authenticated mode requires — and must keep requiring — to be authorized.
    let index = bind(1u32);
    assert!(matches!(index, AuthRef::Inline(_)));

    let picked = select!(Address, addresses[index]);
    let city = select!(String, picked.city);
    assert_eq!(greet(city), "Hello, Madrid!");
}

/// A draft builds field by field and finalizes to the value, with no root
/// hashing and no transition witness behind it (§7).
#[test]
fn a_draft_finalizes_to_its_value() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter("a_draft_finalizes_to_its_value");

    let mut draft: raster::Draft<CollectiveGreeting> = new!(CollectiveGreeting);
    draft.title().set("Built unauthenticated".to_string());
    draft.lines().push("first".to_string());
    draft.lines().push("second".to_string());

    let finalized = inline(raster::finalize(draft));
    assert_eq!(finalized.title, "Built unauthenticated");
    assert_eq!(
        finalized.lines.as_slice(),
        ["first".to_string(), "second".to_string()]
    );
}

/// Set-once is a property of the draft, not of authentication, so it still
/// fires with commitments off — `finalize_draft_value` is shared by both modes
/// precisely so these rules cannot drift apart.
#[test]
#[should_panic(expected = "can only be written once")]
fn set_once_still_holds() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter("set_once_still_holds");

    let mut draft: raster::Draft<CollectiveGreeting> = new!(CollectiveGreeting);
    draft.title().set("first".to_string());
    draft.title().set("second".to_string());
}

/// An append-only field left untouched still materializes, which is the
/// empty-recur path (`allow_partial`) reaching `finalize_draft_value` with
/// `require_complete = false`.
#[test]
fn an_untouched_append_field_finalizes_empty() {
    unauthenticated();
    let _guard =
        raster::__private::SequenceScopeGuard::enter("an_untouched_append_field_finalizes_empty");

    let mut draft: raster::Draft<CollectiveGreeting> = new!(CollectiveGreeting);
    draft.title().set("No lines".to_string());

    let finalized = inline(raster::finalize(draft));
    assert_eq!(finalized.title, "No lines");
    assert!(finalized.lines.as_slice().is_empty());
}
