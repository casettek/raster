use raster::core::trace::TraceEvent;
use raster::prelude::*;
use raster::selector_path;
use raster_core::postcard;
use raster_runtime::{init_with, Publisher, Sha256Commitment};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::thread::ThreadId;

fn missing_name_error() -> String {
    String::from("MissingName")
}

static TRACE_CAPTURE_LOCK: Mutex<()> = Mutex::new(());
static TRACE_INIT: Once = Once::new();
static TRACE_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());
static TRACE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static TRACE_CAPTURE_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

struct TestPublisher;

impl Publisher for TestPublisher {
    fn publish(&self, event: TraceEvent) {
        let current_thread = std::thread::current().id();
        let capture_thread = TRACE_CAPTURE_THREAD.lock().unwrap().clone();
        if TRACE_CAPTURE_ACTIVE.load(Ordering::SeqCst) && capture_thread == Some(current_thread) {
            TRACE_EVENTS.lock().unwrap().push(event);
        }
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
    *TRACE_CAPTURE_THREAD.lock().unwrap() = Some(std::thread::current().id());
    TRACE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);

    let result = f();
    let events = TRACE_EVENTS.lock().unwrap().clone();
    TRACE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    *TRACE_CAPTURE_THREAD.lock().unwrap() = None;
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

fn takes_auth_binding(_: AuthRef<PersonalData>) {}
fn takes_auth_name_binding(_: AuthRef<String>) {}

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

#[sequence]
fn capture_echo_reference(name: String) -> InternalRef {
    let echoed = call!(echo_name, name);
    echoed.reference().clone()
}

#[sequence]
fn capture_success_reference(name: String) -> Result<InternalRef> {
    let echoed = call!(maybe_echo_name, name)?;
    Ok(echoed.reference().clone())
}

fn run_echo_sequence<A>(name: A) -> String
where
    A: IntoAuthRef<String>,
{
    materialize_auth_return::<String, _>(__raster_sequence_auth_echo_sequence(name))
}

fn run_maybe_echo_sequence<A>(name: A) -> Result<String>
where
    A: IntoAuthRef<String>,
{
    materialize_auth_result::<String, _>(__raster_sequence_auth_maybe_echo_sequence(name))
}

fn run_capture_echo_reference<A>(name: A) -> InternalRef
where
    A: IntoAuthRef<String>,
{
    materialize_auth_return::<InternalRef, _>(__raster_sequence_auth_capture_echo_reference(name))
}

fn run_capture_success_reference<A>(name: A) -> Result<InternalRef>
where
    A: IntoAuthRef<String>,
{
    materialize_auth_result::<InternalRef, _>(__raster_sequence_auth_capture_success_reference(name))
}

fn run_traced_error_outer<A>(name: A) -> Result<String>
where
    A: IntoAuthRef<String>,
{
    materialize_auth_result::<String, _>(__raster_sequence_auth_traced_error_outer(name))
}

#[test]
fn select_accepts_identity_typed_external() {
    takes_auth_binding(select!(
        PersonalData,
        external!(PersonalData, "personal_data")
    ));
}

#[test]
fn select_accepts_nested_identity_selected_external() {
    let whole = select!(PersonalData, external!(PersonalData, "personal_data"));
    takes_auth_name_binding(select!(String, whole.name));
}

#[test]
fn select_accepts_nested_selected_external() {
    let address = select!(Address, external!(PersonalData, "personal_data").address);
    takes_auth_name_binding(select!(String, address.line));
}

#[test]
fn auth_ref_preserves_external_binding() {
    takes_auth_binding(into_auth_ref::<PersonalData, _>(external!(
        PersonalData,
        "personal_data"
    )));
}

#[test]
fn select_accepts_auth_ref_binding() {
    let personal = into_auth_ref::<PersonalData, _>(external!(PersonalData, "personal_data"));
    takes_auth_name_binding(select!(String, personal.name));
}

#[test]
fn select_accepts_cloned_auth_ref_binding() {
    let personal = into_auth_ref::<PersonalData, _>(external!(PersonalData, "personal_data"));
    takes_auth_name_binding(select!(String, personal.clone().name));
    takes_auth_binding(personal);
}

#[test]
fn nested_auth_ref_selection_matches_direct_external_selection_trace() {
    let personal = PersonalData {
        name: "Raster".to_string(),
        address: Address {
            line: "Main Street".to_string(),
        },
    };
    let root_hash = vec![1, 2, 3, 4];
    let whole = ExternalValue::new(
        "personal_data",
        SelectorPath::default(),
        Some("commitment".to_string()),
        SelectedPayload {
            bytes: postcard::to_allocvec(&personal).unwrap(),
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: root_hash.clone(),
                steps: Vec::new(),
            },
        },
        personal,
    );
    let address_selector = selector_path(vec![SelectorSegment::Field("address".to_string())]);
    let line_selector = selector_path(vec![SelectorSegment::Field("line".to_string())]);
    let full_selector = selector_path(vec![
        SelectorSegment::Field("address".to_string()),
        SelectorSegment::Field("line".to_string()),
    ]);

    let address = raster::input::select_external_value::<PersonalData, Address>(
        &whole,
        &address_selector,
        &address_selector,
    )
    .unwrap();
    let nested = raster::input::select_external_value::<Address, String>(
        &address,
        &line_selector,
        &full_selector,
    )
    .unwrap();
    let direct = raster::input::select_external_value::<PersonalData, String>(
        &whole,
        &full_selector,
        &full_selector,
    )
    .unwrap();

    assert_eq!(nested.selector, direct.selector);
    assert_eq!(nested.selected, direct.selected);
    assert_eq!(nested.value, direct.value);
    assert_eq!(nested.selected.proof.root_hash, root_hash);
}

#[test]
fn tile_wrapper_accepts_inline_arguments() {
    assert_eq!(echo_name("Raster".to_string()), "Raster");
}

#[test]
fn sequence_wrapper_accepts_inline_arguments() {
    assert_eq!(run_echo_sequence("Raster".to_string()), "Raster");
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
        run_maybe_echo_sequence("Raster".to_string()),
        Ok("Raster".to_string())
    );
    assert_eq!(
        run_maybe_echo_sequence(String::new()),
        Err(missing_name_error())
    );
}

#[test]
fn infallible_call_binding_uses_tile_output_commitment() {
    let (reference, events) = capture_trace_events(|| run_capture_echo_reference("Raster".to_string()));
    let tile_event = events
        .into_iter()
        .find(
            |event| matches!(event, TraceEvent::TileExec(record) if record.fn_name == "echo_name"),
        )
        .expect("expected echo_name tile event");

    let TraceEvent::TileExec(record) = tile_event else {
        panic!("expected tile event");
    };
    let output = record.output.expect("tile output should be recorded");

    assert_eq!(
        reference.coordinates,
        raster::core::cfs::CfsCoordinates(vec![0])
    );
    assert_eq!(
        reference.commitment,
        Into::<Vec<u8>>::into(Sha256Commitment::from(output.data.as_slice()))
    );
    assert_eq!(
        raster::resolve_internal_value::<String>(reference)
            .unwrap()
            .into_inner(),
        "Raster"
    );
}

#[test]
fn fallible_call_binding_resolves_ok_payload_from_stored_result() {
    let (reference, events) =
        capture_trace_events(|| run_capture_success_reference("Raster".to_string()).unwrap());
    let tile_event = events
        .into_iter()
        .find(
            |event| matches!(event, TraceEvent::TileExec(record) if record.fn_name == "maybe_echo_name"),
        )
        .expect("expected maybe_echo_name tile event");

    let TraceEvent::TileExec(record) = tile_event else {
        panic!("expected tile event");
    };
    let output = record.output.expect("tile output should be recorded");
    let decoded: raster::exec::Result<String> = postcard::from_bytes(&output.data).unwrap();

    assert_eq!(decoded, Ok("Raster".to_string()));
    assert_eq!(
        reference.coordinates,
        raster::core::cfs::CfsCoordinates(vec![0])
    );
    assert_eq!(
        reference.commitment,
        Into::<Vec<u8>>::into(Sha256Commitment::from(output.data.as_slice()))
    );
    assert_eq!(
        raster::resolve_internal_ok_value::<String>(reference)
            .unwrap()
            .into_inner(),
        "Raster"
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
    let (result, events) = capture_trace_events(|| run_traced_error_outer("Raster".to_string()));
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
#[should_panic(expected = "Failed to materialize auth value for argument 'name'")]
fn tile_wrapper_panics_on_runtime_resolution_failure() {
    let _ = maybe_echo_name(external!(String, "missing_name"));
}

#[test]
#[should_panic(expected = "Failed to trace sequence argument 'name'")]
fn sequence_wrapper_panics_on_runtime_trace_failure() {
    let _ = run_maybe_echo_sequence(external!(String, "missing_name"));
}

#[test]
fn zero_arg_sequence_wrapper_accepts_no_arguments() {
    __raster_sequence_auth_zero_arg_sequence();
}
