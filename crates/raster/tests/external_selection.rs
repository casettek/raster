use raster::core::trace::TraceEvent;
use raster::prelude::*;
use raster_core::postcard;
use raster_runtime::{init_with, Publisher};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, Once};

fn missing_name_error() -> String {
    String::from("MissingName")
}

static TRACE_CAPTURE_LOCK: Mutex<()> = Mutex::new(());
static TRACE_INIT: Once = Once::new();
static TRACE_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());

struct TestPublisher;

impl Publisher for TestPublisher {
    fn publish(&self, event: TraceEvent) {
        TRACE_EVENTS.lock().unwrap().push(event);
    }

    fn finish(&self) {}
}

fn capture_trace_events<F, T>(f: F) -> (T, Vec<TraceEvent>)
where
    F: FnOnce() -> T,
{
    let _guard = TRACE_CAPTURE_LOCK.lock().unwrap();
    TRACE_INIT.call_once(|| init_with(TestPublisher));
    TRACE_EVENTS.lock().unwrap().clear();

    let result = f();
    let events = TRACE_EVENTS.lock().unwrap().clone();
    (result, events)
}

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
        Err(missing_name_error())
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
    let echoed = call!(maybe_echo_name, name)?;
    Ok(echoed)
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

#[sequence]
fn traced_error_inner(name: String) -> Result<String> {
    let echoed = call!(echo_name, name);
    let _ = call!(maybe_echo_name, String::new())?;
    Ok(echoed)
}

#[sequence]
fn traced_error_outer(name: String) -> Result<String> {
    let echoed = call!(echo_name, name);
    let inner = call_seq!(traced_error_inner, echoed)?;
    Ok(inner)
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
    assert_eq!(maybe_echo_name(String::new()), Err(missing_name_error()));
}

#[test]
fn sequence_wrapper_preserves_user_result() {
    assert_eq!(
        maybe_echo_sequence("Raster".to_string()),
        Ok("Raster".to_string())
    );
    assert_eq!(
        maybe_echo_sequence(String::new()),
        Err(missing_name_error())
    );
}

#[test]
fn tile_abi_wrapper_serializes_user_error_result() {
    let input = postcard::to_allocvec(&String::new()).unwrap();
    let output = __raster_tile_entry_maybe_echo_name(&input).unwrap();
    let decoded: raster::exec::Result<String> = postcard::from_bytes(&output).unwrap();
    assert_eq!(decoded, Err(missing_name_error()));
}

#[test]
fn nested_sequence_trace_records_terminal_err_outputs() {
    let (result, events) = capture_trace_events(|| traced_error_outer("Raster".to_string()));
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| match event {
            TraceEvent::SequenceStart(record) | TraceEvent::SequenceEnd(record) => {
                matches!(
                    record.fn_name.as_str(),
                    "traced_error_outer" | "traced_error_inner"
                )
            }
            TraceEvent::TileExec(record) => {
                matches!(record.fn_name.as_str(), "echo_name" | "maybe_echo_name")
            }
        })
        .collect();

    fn matches_expected_shape(event: &TraceEvent, index: usize) -> bool {
        match (index, event) {
            (0, TraceEvent::SequenceStart(record)) => record.fn_name == "traced_error_outer",
            (1, TraceEvent::TileExec(record)) => record.fn_name == "echo_name",
            (2, TraceEvent::SequenceStart(record)) => record.fn_name == "traced_error_inner",
            (3, TraceEvent::TileExec(record)) => record.fn_name == "echo_name",
            (4, TraceEvent::TileExec(record)) => record.fn_name == "maybe_echo_name",
            (5, TraceEvent::SequenceEnd(record)) => record.fn_name == "traced_error_inner",
            (6, TraceEvent::SequenceEnd(record)) => record.fn_name == "traced_error_outer",
            _ => false,
        }
    }

    let start_idx = events
        .windows(7)
        .position(|window| {
            window
                .iter()
                .enumerate()
                .all(|(idx, event)| matches_expected_shape(event, idx))
        })
        .expect("expected traced error path in captured events");
    let events = events[start_idx..start_idx + 7].to_vec();

    assert_eq!(result, Err(missing_name_error()));
    assert_eq!(events.len(), 7);

    match &events[4] {
        TraceEvent::TileExec(record) => {
            assert_eq!(record.fn_name, "maybe_echo_name");
            let output = record.output.as_ref().unwrap();
            let decoded: raster::exec::Result<String> = postcard::from_bytes(&output.data).unwrap();
            assert_eq!(decoded, Err(missing_name_error()));
        }
        other => panic!("expected failing tile event, got {:?}", other),
    }

    match &events[5] {
        TraceEvent::SequenceEnd(record) => {
            assert_eq!(record.fn_name, "traced_error_inner");
            let output = record.output.as_ref().unwrap();
            let decoded: raster::exec::Result<String> = postcard::from_bytes(&output.data).unwrap();
            assert_eq!(decoded, Err(missing_name_error()));
        }
        other => panic!("expected inner sequence end, got {:?}", other),
    }

    match &events[6] {
        TraceEvent::SequenceEnd(record) => {
            assert_eq!(record.fn_name, "traced_error_outer");
            let output = record.output.as_ref().unwrap();
            let decoded: raster::exec::Result<String> = postcard::from_bytes(&output.data).unwrap();
            assert_eq!(decoded, Err(missing_name_error()));
        }
        other => panic!("expected outer sequence end, got {:?}", other),
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
