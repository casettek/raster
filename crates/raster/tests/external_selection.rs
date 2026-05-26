use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Address {
    line: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct PersonalData {
    name: String,
    address: Address,
}

fn takes_typed_binding(_: TypedSelectedExternalBinding<PersonalData, PersonalData>) {}
fn takes_name_binding(_: TypedSelectedExternalBinding<PersonalData, String>) {}
fn takes_sequence_binding<Root>(_: SequenceArg<Root, PersonalData>) {}
fn takes_sequence_name_binding<Root>(_: SequenceArg<Root, String>) {}

#[tile(kind = iter)]
fn echo_name(name: String) -> String {
    name
}

#[sequence]
fn echo_sequence(name: String) -> String {
    call!(echo_name, name)
}

#[sequence]
fn select_name_from_personal(personal: PersonalData) -> String {
    let name = select!(String, personal.name);
    call!(echo_name, name)
}

#[sequence]
fn forward_personal_binding(personal: PersonalData) -> String {
    call_seq!(select_name_from_personal, personal)
}

#[test]
fn select_accepts_identity_typed_external() {
    takes_typed_binding(select!(
        PersonalData,
        external!(PersonalData, "personal_data")
    ));
}

#[test]
fn select_accepts_nested_identity_selected_external() {
    let whole = select!(PersonalData, external!(PersonalData, "personal_data"));
    takes_name_binding(select!(String, whole.name));
}

#[test]
fn select_accepts_nested_selected_external() {
    let address = select!(Address, external!(PersonalData, "personal_data").address);
    takes_name_binding(select!(String, address.line));
}

#[test]
fn sequence_carrier_preserves_external_binding() {
    takes_sequence_binding(into_sequence_arg::<PersonalData, _>(external!(
        PersonalData,
        "personal_data"
    )));
}

#[test]
fn select_accepts_sequence_preserved_binding() {
    let personal = into_sequence_arg::<PersonalData, _>(external!(PersonalData, "personal_data"));
    takes_sequence_name_binding(select!(String, personal.name));
}

#[test]
fn tile_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_name("Raster".to_string()), "Raster");
}

#[test]
fn sequence_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_sequence("Raster".to_string()), "Raster");
}
