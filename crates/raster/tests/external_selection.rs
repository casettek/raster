use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable, Merklized)]
struct PersonalData {
    name: String,
}

fn takes_typed_binding(_: TypedSelectedExternalBinding<PersonalData, PersonalData>) {}

#[tile(kind = iter)]
fn echo_name(name: String) -> String {
    name
}

#[sequence]
fn echo_sequence(name: String) -> String {
    call!(echo_name, name)
}

#[test]
fn select_accepts_identity_typed_external() {
    takes_typed_binding(select!(
        PersonalData,
        external!(PersonalData, "personal_data")
    ));
}

#[test]
fn tile_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_name("Raster".to_string()), "Raster");
}

#[test]
fn sequence_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_sequence("Raster".to_string()), "Raster");
}
