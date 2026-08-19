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

/// v1 refuses drafts rather than half-supporting them (§7), naming the
/// construct to blame.
#[test]
#[should_panic(expected = "`new!` requires authenticated execution")]
fn drafts_are_refused() {
    unauthenticated();
    let _guard = raster::__private::SequenceScopeGuard::enter("drafts_are_refused");
    let _draft: raster::Draft<CollectiveGreeting> = new!(CollectiveGreeting);
}
