use raster::prelude::*;
use raster_core::{postcard, TileOutputEnvelope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Error {
    MissingName,
}

type Result<T> = std::result::Result<T, Error>;

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

#[tile(kind = iter)]
fn maybe_echo_name(name: String) -> Result<String> {
    if name.is_empty() {
        Err(Error::MissingName)
    } else {
        Ok(name)
    }
}

#[sequence]
fn echo_sequence(name: String) -> String {
    call!(echo_name, name)
}

#[sequence]
fn maybe_echo_sequence(name: String) -> Result<String> {
    call!(maybe_echo_name, name)
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

#[sequence]
fn zero_arg_sequence() {
    let _ = call!(echo_name, "Raster".to_string());
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
fn select_accepts_cloned_sequence_preserved_binding() {
    let personal = into_sequence_arg::<PersonalData, _>(external!(PersonalData, "personal_data"));
    takes_sequence_name_binding(select!(String, personal.clone().name));
    takes_sequence_binding(personal);
}

#[test]
fn tile_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_name("Raster".to_string()), "Raster");
}

#[test]
fn sequence_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_sequence("Raster".to_string()), "Raster");
}

#[test]
fn tile_wrapper_preserves_user_result() {
    assert_eq!(
        maybe_echo_name("Raster".to_string()),
        Ok("Raster".to_string())
    );
    assert_eq!(maybe_echo_name(String::new()), Err(Error::MissingName));
}

#[test]
fn sequence_wrapper_preserves_user_result() {
    assert_eq!(
        maybe_echo_sequence("Raster".to_string()),
        Ok("Raster".to_string())
    );
    assert_eq!(maybe_echo_sequence(String::new()), Err(Error::MissingName));
}

#[test]
fn tile_abi_wrapper_serializes_user_error_result() {
    let input = postcard::to_allocvec(&String::new()).unwrap();
    let output = __raster_tile_entry_maybe_echo_name(&input).unwrap();
    let decoded: TileOutputEnvelope = postcard::from_bytes(&output).unwrap();
    match decoded {
        TileOutputEnvelope::UserError { bytes, display } => {
            let decoded_error: Error = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(decoded_error, Error::MissingName);
            assert!(display.contains("MissingName"));
        }
        other => panic!("expected user error envelope, got {:?}", other),
    }
}

#[test]
#[should_panic(expected = "Failed to resolve call argument 'name'")]
fn tile_wrapper_panics_on_runtime_resolution_failure() {
    let _ = maybe_echo_name(external!(String, "missing_name"));
}

#[test]
#[should_panic(expected = "Failed to trace sequence argument 'name'")]
fn sequence_wrapper_panics_on_runtime_trace_failure() {
    let _ = maybe_echo_sequence(external!(String, "missing_name"));
}

#[test]
fn zero_arg_sequence_wrapper_accepts_no_arguments() {
    zero_arg_sequence();
}
