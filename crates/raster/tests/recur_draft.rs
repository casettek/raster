use raster::core::draft::{apply_draft_ops, verify_witness_root, DraftReplayHandle};
use raster::core::trace::{FnInputValue, TraceEvent};
use raster::into_auth_value;
use raster::materialize_auth_return;
use raster::prelude::*;
use raster_runtime::{init_with, Publisher};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, Once};
use std::thread::ThreadId;

static TRACE_CAPTURE_LOCK: Mutex<()> = Mutex::new(());
static TRACE_INIT: Once = Once::new();
static TRACE_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());
static TRACE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static TRACE_CAPTURE_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);
/// Whole-source resolutions counted by the recur-*tile* fixture, and by the
/// recur-*sequence* one, separately.
///
/// One shared counter would race: both tests reset it and then assert on it,
/// and `cargo test` runs them in parallel by default. That was latent while
/// both expected the same value.
static RECUR_TILE_RESOLVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static RECUR_SEQUENCE_RESOLVE_COUNT: AtomicUsize = AtomicUsize::new(0);

struct TestPublisher;

impl Publisher for TestPublisher {
    fn publish(&self, event: TraceEvent) {
        let current_thread = std::thread::current().id();
        let capture_thread = TRACE_CAPTURE_THREAD.lock().unwrap().to_owned();
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
struct LineBundle {
    title: String,
    items: List<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct SearchBundle {
    needle: String,
    matches: List<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct UnitLike;

impl Selectable for UnitLike {
    fn schema() -> SchemaNode {
        SchemaNode::Leaf {
            type_name: "UnitLike".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Selectable)]
struct UnitLineBundle {
    marker: UnitLike,
    items: List<String>,
}

// Output schema with no required scalar fields, so a recur sequence can
// finalize without the step body writing anything (used by the resolve-count
// guard, where the step only materializes items).
#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct ItemsOnlyBundle {
    items: List<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct LimitedBundle {
    limit: u64,
    stopped_after: u64,
    items: List<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LimitState {
    seen: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Selectable)]
struct MaxLenState {
    max_len: u64,
}

#[tile(kind = recur)]
fn collect_lines(
    input: RecurInput<String>,
    output: RecurOutput<LineBundle>,
) -> RecurOutput<LineBundle> {
    let mut output = output;
    if input.is_first() {
        output.title().set("collected".to_string());
    }
    output.items().push(input.into_value());
    output
}

#[tile(kind = recur)]
fn collect_first_match(
    input: RecurInput<String>,
    output: RecurOutput<SearchBundle>,
    needle: String,
) -> RecurControl<RecurOutput<SearchBundle>> {
    let mut output = output;
    if input.is_first() {
        output.needle().set(needle.clone());
    }
    let item = input.into_value();
    if item == needle {
        output.matches().push(item);
        RecurControl::Break(output)
    } else {
        RecurControl::Continue(output)
    }
}

#[tile(kind = recur)]
fn collect_optional_lines(
    input: RecurInput<String>,
    output: RecurOutput<UnitLineBundle>,
) -> RecurOutput<UnitLineBundle> {
    let mut output = output;
    if input.is_first() {
        output.marker().set(UnitLike);
    }
    output.items().push(input.into_value());
    output
}

#[sequence]
fn build_lines_reference() -> StorageRef {
    let source = raster::store_value(&vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = collect_lines,
        input = storage!(List<String>, source),
        output = new!(LineBundle),
        args = ()
    )
    .reference()
    .clone()
}

fn run_build_lines_reference() -> StorageRef {
    materialize_auth_return::<StorageRef, _>(__raster_sequence_auth_build_lines_reference())
}

#[sequence]
fn find_first_match(needle: String) -> SearchBundle {
    let source = raster::store_value(&vec![
        "alpha".to_string(),
        "beta".to_string(),
        "gamma".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = collect_first_match,
        input = storage!(List<String>, source),
        output = new!(SearchBundle),
        args = (needle,)
    )
}

#[sequence]
fn collect_optional_lines_from_empty() -> UnitLineBundle {
    let source = raster::store_value(&Vec::<String>::new()).expect("list source should store");

    call_recur!(
        tile = collect_optional_lines,
        input = storage!(List<String>, source),
        output = new!(UnitLineBundle),
        args = ()
    )
}

#[sequence]
fn collect_required_lines_from_empty() -> LineBundle {
    let source = raster::store_value(&Vec::<String>::new()).expect("list source should store");

    call_recur!(
        tile = collect_lines,
        input = storage!(List<String>, source),
        output = new!(LineBundle),
        args = ()
    )
}

fn run_find_first_match(needle: String) -> SearchBundle {
    materialize_auth_return::<SearchBundle, _>(__raster_sequence_auth_find_first_match(needle))
}

fn run_collect_optional_lines_from_empty() -> UnitLineBundle {
    materialize_auth_return::<UnitLineBundle, _>(
        __raster_sequence_auth_collect_optional_lines_from_empty(),
    )
}

fn run_collect_required_lines_from_empty() -> LineBundle {
    materialize_auth_return::<LineBundle, _>(
        __raster_sequence_auth_collect_required_lines_from_empty(),
    )
}

#[tile(kind = recur)]
fn collect_until_limit(
    input: RecurInput<String>,
    state: RecurState<LimitState>,
    output: RecurOutput<LimitedBundle>,
    limit: u64,
) -> RecurControl<(RecurState<LimitState>, RecurOutput<LimitedBundle>)> {
    let mut state = state;
    let mut output = output;
    if input.is_first() {
        output.limit().set(limit);
    }

    state.seen += 1;
    output.items().push(input.into_value());

    if state.seen >= limit {
        output.stopped_after().set(state.seen);
        RecurControl::Break((state, output))
    } else {
        RecurControl::Continue((state, output))
    }
}

#[tile(kind = recur)]
fn track_max_len(
    input: RecurInput<String>,
    state: RecurState<MaxLenState>,
) -> RecurState<MaxLenState> {
    let mut state = state;
    let len = input.value().len() as u64;
    if len > state.max_len {
        state.max_len = len;
    }
    state
}

#[tile(kind = recur)]
fn count_until_limit_state_only(
    input: RecurInput<String>,
    state: RecurState<LimitState>,
    limit: u64,
) -> RecurControl<RecurState<LimitState>> {
    let _ = input;
    let mut state = state;
    state.seen += 1;

    if state.seen >= limit {
        RecurControl::Break(state)
    } else {
        RecurControl::Continue(state)
    }
}

#[tile]
fn prefix_line(line: String, prefix: String) -> String {
    format!("{}{}", prefix, line)
}

#[tile]
fn init_prefixed_bundle(output: Draft<LineBundle>) -> Draft<LineBundle> {
    let mut output = output;
    output.title().set("prefixed".to_string());
    output
}

#[tile]
fn append_prefixed_line(output: Draft<LineBundle>, line: String) -> Draft<LineBundle> {
    let mut output = output;
    output.items().push(line);
    output
}

#[sequence(kind = recur)]
fn collect_prefixed_lines(
    input: RecurSequenceInput<String>,
    output: RecurSequenceOutput<LineBundle>,
    prefix: String,
) -> RecurSequenceOutput<LineBundle> {
    let line = call!(prefix_line, input, prefix);
    call!(append_prefixed_line, output, line)
}

#[sequence]
fn collect_two_items(limit: u64) -> LimitedBundle {
    let source = raster::store_value(&vec![
        "one".to_string(),
        "two".to_string(),
        "three".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = collect_until_limit,
        input = storage!(List<String>, source),
        state = LimitState { seen: 0 },
        output = new!(LimitedBundle),
        args = (limit,)
    )
}

#[sequence]
fn compute_max_len() -> MaxLenState {
    let source = raster::store_value(&vec![
        "a".to_string(),
        "alphabet".to_string(),
        "rust".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = track_max_len,
        input = storage!(List<String>, source),
        state = MaxLenState { max_len: 0 },
        args = ()
    )
}

#[sequence]
fn compute_max_len_field() -> u64 {
    let source = raster::store_value(&vec![
        "a".to_string(),
        "alphabet".to_string(),
        "rust".to_string(),
    ])
    .expect("list source should store");

    let stats = call_recur!(
        tile = track_max_len,
        input = storage!(List<String>, source),
        state = MaxLenState { max_len: 0 },
        args = ()
    );

    select!(u64, stats.max_len)
}

#[sequence]
fn count_seen_until_limit(limit: u64) -> LimitState {
    let source = raster::store_value(&vec![
        "one".to_string(),
        "two".to_string(),
        "three".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = count_until_limit_state_only,
        input = storage!(List<String>, source),
        state = LimitState { seen: 0 },
        args = (limit,)
    )
}

#[sequence]
fn state_only_empty_input() -> MaxLenState {
    let source = raster::store_value(&Vec::<String>::new()).expect("list source should store");

    call_recur!(
        tile = track_max_len,
        input = storage!(List<String>, source),
        state = MaxLenState { max_len: 0 },
        args = ()
    )
}

#[sequence]
fn build_prefixed_lines_with_recur_sequence() -> LineBundle {
    let source = raster::store_value(&vec![
        "one".to_string(),
        "two".to_string(),
        "three".to_string(),
    ])
    .expect("list source should store");
    let prefix_source =
        raster::store_value(&"line: ".to_string()).expect("prefix source should store");

    let output = call!(init_prefixed_bundle, new!(LineBundle));

    call_recur_seq!(
        sequence = collect_prefixed_lines,
        input = storage!(List<String>, source),
        output = output,
        args = (storage!(String, prefix_source),)
    )
}

fn run_collect_two_items(limit: u64) -> LimitedBundle {
    materialize_auth_return::<LimitedBundle, _>(__raster_sequence_auth_collect_two_items(limit))
}

fn run_compute_max_len() -> MaxLenState {
    materialize_auth_return::<MaxLenState, _>(__raster_sequence_auth_compute_max_len())
}

fn run_compute_max_len_field() -> u64 {
    materialize_auth_return::<u64, _>(__raster_sequence_auth_compute_max_len_field())
}

fn run_count_seen_until_limit(limit: u64) -> LimitState {
    materialize_auth_return::<LimitState, _>(__raster_sequence_auth_count_seen_until_limit(limit))
}

fn run_state_only_empty_input() -> MaxLenState {
    materialize_auth_return::<MaxLenState, _>(__raster_sequence_auth_state_only_empty_input())
}

fn run_build_prefixed_lines_with_recur_sequence() -> LineBundle {
    materialize_auth_return::<LineBundle, _>(
        __raster_sequence_auth_build_prefixed_lines_with_recur_sequence(),
    )
}

fn resolve_counted_string_list(
    reference: StorageRef,
) -> raster::core::Result<StorageValue<List<String>>> {
    RECUR_TILE_RESOLVE_COUNT.fetch_add(1, Ordering::SeqCst);
    raster::resolve_storage_value::<List<String>>(reference)
}

fn resolve_counted_string_list_for_sequence(
    reference: StorageRef,
) -> raster::core::Result<StorageValue<List<String>>> {
    RECUR_SEQUENCE_RESOLVE_COUNT.fetch_add(1, Ordering::SeqCst);
    raster::resolve_storage_value::<List<String>>(reference)
}

#[test]
fn call_recur_finalizes_to_selectable_internal_ref() {
    let reference = run_build_lines_reference();

    let title = select!(String, storage!(LineBundle, reference.clone()).title);
    let first = select!(String, storage!(LineBundle, reference.clone()).items[0]);
    let third = select!(String, storage!(LineBundle, reference).items[2]);

    assert_eq!(
        into_auth_value::<String, _>(title).unwrap().into_inner(),
        "collected"
    );
    assert_eq!(
        into_auth_value::<String, _>(first).unwrap().into_inner(),
        "first"
    );
    assert_eq!(
        into_auth_value::<String, _>(third).unwrap().into_inner(),
        "third"
    );
}

#[test]
fn debug_formats_materialized_internal_auth_ref() {
    let reference = run_build_lines_reference();
    let auth = into_auth_ref::<LineBundle, _>(storage!(LineBundle, reference));

    let rendered = format!("{auth:?}");

    assert!(rendered.contains("AuthRef"));
    assert!(rendered.contains("storage: \"storage\""));
    assert!(rendered.contains("coordinates: \""));
    assert!(rendered.contains("commitment_len"));
    assert!(rendered.contains("stored_bytes_len"));
    assert!(rendered.contains("title: \"collected\""));
    assert!(rendered.contains("items: [\"first\", \"second\", \"third\"]"));
}

#[test]
fn call_recur_breaks_early_and_still_finalizes() {
    let result = run_find_first_match("beta".to_string());

    assert_eq!(result.needle, "beta");
    assert_eq!(result.matches.into_vec(), vec!["beta".to_string()]);
}

#[test]
fn call_recur_empty_input_materializes_optional_fields() {
    let result = run_collect_optional_lines_from_empty();

    assert_eq!(result.marker, UnitLike);
    assert!(result.items.is_empty());
}

#[test]
#[should_panic(expected = "field 'title' was never written")]
fn call_recur_empty_input_surfaces_targeted_error_for_required_fields() {
    let _ = run_collect_required_lines_from_empty();
}

#[test]
fn call_recur_threads_state_and_finalizes() {
    let result = run_collect_two_items(2);

    assert_eq!(result.limit, 2);
    assert_eq!(result.stopped_after, 2);
    assert_eq!(
        result.items.into_vec(),
        vec!["one".to_string(), "two".to_string()]
    );
}

#[test]
fn call_recur_can_return_state_only_results() {
    let result = run_compute_max_len();

    assert_eq!(result.max_len, 8);
}

#[test]
fn call_recur_state_only_results_can_be_selected() {
    let result = run_compute_max_len_field();

    assert_eq!(result, 8);
}

#[test]
fn call_recur_state_only_break_returns_final_state() {
    let result = run_count_seen_until_limit(2);

    assert_eq!(result.seen, 2);
}

#[test]
fn call_recur_state_only_empty_input_returns_initial_state() {
    let result = run_state_only_empty_input();

    assert_eq!(result.max_len, 0);
}

#[test]
fn call_recur_seq_orchestrates_tiles_per_item() {
    let result = run_build_prefixed_lines_with_recur_sequence();

    assert_eq!(result.title, "prefixed");
    assert_eq!(
        result.items.into_vec(),
        vec![
            "line: one".to_string(),
            "line: two".to_string(),
            "line: three".to_string(),
        ]
    );
}

/// A recur tile never materializes its source either.
///
/// This is the bigger of the two changes: recur tiles previously bypassed
/// per-item selection entirely, resolving the source into an owned `Vec<T>` and
/// iterating that — the index was not consulted once. They now run on the same
/// `ListCursor` the sequence family does, so the two families share one
/// implementation and neither has a path back to `O(list)`.
#[test]
fn call_recur_never_materializes_its_source() {
    let _guard = raster::__private::SequenceScopeGuard::enter("recur_single_list_resolve");
    let reference = raster::store_value(&vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ])
    .expect("list source should store");
    let source = into_auth_ref::<List<String>, _>(raster::typed_storage_with_resolver::<
        List<String>,
    >(reference, resolve_counted_string_list));

    RECUR_TILE_RESOLVE_COUNT.store(0, Ordering::SeqCst);
    let auth = raster::run_recur_list::<String, LineBundle, _, _>(
        source,
        new!(LineBundle),
        |input, output| collect_lines(input, output),
    );
    let result = into_auth_value::<LineBundle, _>(auth).unwrap().into_inner();

    // The sweep really ran — otherwise "zero resolves" would be vacuous.
    assert_eq!(result.title, "collected");
    assert_eq!(result.items.len(), 3);
    assert_eq!(
        RECUR_TILE_RESOLVE_COUNT.load(Ordering::SeqCst),
        0,
        "the source list must never be resolved whole",
    );
}

/// A recur sequence never materializes its source — not once, not at all.
///
/// This used to assert exactly *one* whole-source resolve: the driver resolved
/// the parent list up front and cached it behind an `Rc` so per-item `AuthRef`s
/// could select out of the cached value. `ListCursor` removes that cache along
/// with the resolve it was caching — the loop bound now comes from the `0x0A`
/// metadata selection and each item from its own indexed read — so the honest
/// expectation is zero. See `docs/proposals/lazy-list-recur.md` §2–§3.
#[test]
fn recur_sequence_never_materializes_its_source() {
    let _guard = raster::__private::SequenceScopeGuard::enter("recur_sequence_single_list_resolve");
    let reference = raster::store_value(&vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ])
    .expect("list source should store");
    let source = into_auth_ref::<List<String>, _>(raster::typed_storage_with_resolver::<
        List<String>,
    >(reference, resolve_counted_string_list_for_sequence));

    RECUR_SEQUENCE_RESOLVE_COUNT.store(0, Ordering::SeqCst);
    // Materialize every item the way the trace pipeline does, so this counts
    // the *source* resolutions a full sweep costs — not just the ones an
    // untouched `AuthRef` would defer.
    let mut items = 0usize;
    let _auth = raster::run_recur_sequence_list::<String, ItemsOnlyBundle, _, _>(
        source,
        new!(ItemsOnlyBundle),
        |input, output| {
            input
                .__raster_auth_trace()
                .expect("sequence item should resolve");
            items += 1;
            output
        },
    );

    // The sweep really ran — otherwise "zero resolves" would be vacuous.
    assert_eq!(items, 3, "every element should still be visited");
    assert_eq!(
        RECUR_SEQUENCE_RESOLVE_COUNT.load(Ordering::SeqCst),
        0,
        "the source list must never be resolved whole",
    );
}

#[test]
fn recur_trace_serializes_non_reusable_draft_markers() {
    let (_reference, events) = capture_trace_events(run_build_lines_reference);
    let collect_lines_event = events
        .into_iter()
        .find_map(|event| match event {
            TraceEvent::RecurTileIterationExec(record) if record.fn_name == "collect_lines" => {
                Some(record)
            }
            _ => None,
        })
        .expect("collect_lines trace should be recorded");
    let input = collect_lines_event
        .input
        .expect("collect_lines input should be traced");
    let draft_bytes = match input.values.get(1) {
        Some(FnInputValue::Inline(bytes)) => bytes.clone(),
        other => panic!("expected traced output draft marker, found {:?}", other),
    };
    let replay_handle: DraftReplayHandle = raster::core::postcard::from_bytes(&draft_bytes)
        .expect("draft trace should encode replay handle");

    assert!(raster::core::postcard::from_bytes::<Draft<LineBundle>>(&draft_bytes).is_err());
    assert_eq!(replay_handle.schema_hash, LineBundle::schema_hash());
}

#[test]
fn recur_trace_threads_verified_roots_between_steps() {
    let (_reference, events) = capture_trace_events(run_build_lines_reference);
    let collect_lines_events: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            TraceEvent::RecurTileIterationExec(record) if record.fn_name == "collect_lines" => {
                Some(record)
            }
            _ => None,
        })
        .collect();

    assert_eq!(collect_lines_events.len(), 3);
    let mut prior_root_after = None;
    for record in collect_lines_events {
        let input = record.input.expect("tile input should be traced");
        let handle_bytes = match input.values.get(1) {
            Some(FnInputValue::Inline(bytes)) => bytes.clone(),
            other => panic!("expected traced draft replay handle, found {:?}", other),
        };
        let handle: DraftReplayHandle = raster::core::postcard::from_bytes(&handle_bytes)
            .expect("draft handle should deserialize");
        let witness = record
            .draft_transition_witness
            .expect("tile trace should include draft witness");
        let native_transition = witness
            .native_transition
            .expect("tile trace should include native draft transition");

        assert_eq!(native_transition.root_before, handle.root_before);
        verify_witness_root(&witness.pre_state, &handle.root_before)
            .expect("pre-state witness should authenticate the replay handle root");

        if let Some(previous_root_after) = prior_root_after {
            assert_eq!(handle.root_before, previous_root_after);
        }

        let (_next_state, root_after) =
            apply_draft_ops(&witness.pre_state, &native_transition.ops).expect("ops should apply");
        prior_root_after = Some(root_after);
    }
}

/// Each iteration records the item it ran on as an authenticated selection at
/// `source[i]`, not as an inline blob.
///
/// Materializing an authorized item produces two things — the value and the
/// binding that authorizes it — and the old `build_recur_input` kept only the
/// first. Recur tiles therefore recorded their item as `FnInputValue::Inline`,
/// with no selector, no commitment and no citations: the audit could see *a*
/// value but nothing tying it to `source[i]`. This asserts both halves of the
/// fix — the trace value is a storage binding, and the recorded selector ends
/// in `Index(i)` for the right `i`. See `docs/proposals/lazy-list-recur.md` §4.
#[test]
fn recur_iterations_record_the_item_binding_they_ran_on() {
    let (_reference, events) = capture_trace_events(run_build_lines_reference);
    let iterations: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            TraceEvent::RecurTileIterationExec(record) if record.fn_name == "collect_lines" => {
                Some(record)
            }
            _ => None,
        })
        .collect();

    assert_eq!(iterations.len(), 3);
    for (index, record) in iterations.into_iter().enumerate() {
        let input = record.input.expect("tile input should be traced");

        assert!(
            matches!(input.values.first(), Some(FnInputValue::StorageBinding)),
            "iteration {} recorded its item as {:?}, not a storage binding",
            index,
            input.values.first(),
        );

        let binding = input
            .storage
            .get("input")
            .unwrap_or_else(|| panic!("iteration {} should record an 'input' binding", index));
        assert_eq!(
            binding.selector.segments.last(),
            Some(&SelectorSegment::Index(index as u64)),
            "iteration {} should be authenticated at source[{}], got {:?}",
            index,
            index,
            binding.selector.segments,
        );
        assert_ne!(
            binding.selection.selected_len, 0,
            "iteration {} binding should carry a real selection commitment",
            index,
        );
    }
}

#[test]
fn recur_trace_emits_site_completion_event() {
    let (_reference, events) = capture_trace_events(run_build_lines_reference);
    let site_events: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            TraceEvent::RecurTileEnd(record) if record.fn_name == "collect_lines" => Some(record),
            _ => None,
        })
        .collect();

    assert_eq!(site_events.len(), 1);
    let site_event = &site_events[0];
    assert!(
        site_event.input.is_some(),
        "recur site should capture input trace"
    );
    assert!(
        site_event.output.is_some(),
        "recur site should capture finalized output"
    );
}

#[test]
fn recur_sequence_trace_keeps_inner_tiles_replayable() {
    let (_result, events) = capture_trace_events(run_build_prefixed_lines_with_recur_sequence);

    #[derive(Debug, Deserialize)]
    struct RecurSequenceInputTrace {
        kind: String,
        index: u64,
        len: u64,
        item: FnInputValue,
    }

    let iteration_start_records = events
        .iter()
        .filter_map(|event| {
            if let TraceEvent::RecurSequenceIterationStart(record) = event {
                (record.fn_name == "collect_prefixed_lines").then_some(record)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let inner_tile_execs = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                TraceEvent::TileExec(record) if record.fn_name == "append_prefixed_line"
            )
        })
        .count();
    let site_completions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                TraceEvent::RecurSequenceEnd(record)
                    if record.fn_name == "collect_prefixed_lines"
            )
        })
        .count();

    assert_eq!(iteration_start_records.len(), 3);
    assert_eq!(inner_tile_execs, 3);
    assert_eq!(site_completions, 1);

    for (expected_index, record) in iteration_start_records.iter().enumerate() {
        let input = record
            .input
            .as_ref()
            .expect("start event should have input");
        assert!(
            input.storage.contains_key("input"),
            "selected item metadata should be keyed by the input parameter name"
        );
        assert!(
            input.storage.contains_key("prefix"),
            "recursive sequence extra args should remain auth refs in iteration traces"
        );
        let FnInputValue::Inline(bytes) = &input.values[0] else {
            panic!("recur sequence input marker should be inline");
        };
        let marker: RecurSequenceInputTrace =
            raster::core::postcard::from_bytes(bytes).expect("marker should decode");
        assert_eq!(marker.kind, "raster::RecurSequenceInput");
        assert_eq!(marker.index, expected_index as u64);
        assert_eq!(marker.len, 3);
        assert_eq!(marker.item, FnInputValue::StorageBinding);
    }
}

// ---- Replay journal: the per-iteration facts (`lazy-list-recur.md` §5) ----
//
// These call the generated replay entry point directly, which is the same
// function the guest runs and whose output the replay receipt covers. Anything
// asserted here is therefore replay-proven in production, not merely recorded
// by the host.

fn replay_journal(bytes: Vec<u8>) -> raster::core::draft::TileReplayJournal {
    raster::core::postcard::from_bytes(&bytes).expect("replay journal should decode")
}

/// An ordinary tile carries no `RecurInput`, so it emits no recur facts at all.
#[test]
fn ordinary_tile_emits_no_recur_journal() {
    let input = raster::core::postcard::to_allocvec(&(
        String::from("line"),
        String::from("prefix: "),
    ))
    .unwrap();
    let journal = replay_journal(__raster_tile_replay_entry_prefix_line(&input).unwrap());
    assert_eq!(journal.recur, None);
}

/// A recur tile whose return type carries no `RecurControl` must still emit an
/// explicit `Continue`.
///
/// The tempting alternative — omit the field and read absence as `Continue` —
/// puts a default in guest audit code, where it is indistinguishable from a
/// field a producer failed to set. `dynamic-index-selection.md` already records
/// what that costs: `InputSource::Inline` was exactly such a permissive
/// fallback. This is the regression test for not reintroducing it.
#[test]
fn recur_tile_without_control_emits_explicit_continue() {
    let input = raster::core::postcard::to_allocvec(&(
        RecurInput::new(String::from("abcd"), 2u64, 5u64),
        RecurState::new(MaxLenState { max_len: 0 }),
    ))
    .unwrap();
    let journal = replay_journal(__raster_tile_replay_entry_track_max_len(&input).unwrap());

    let recur = journal.recur.expect("a recur tile must emit recur facts");
    assert_eq!(recur.control, raster::core::draft::RecurControlKind::Continue);
    assert_eq!(recur.position.iteration_index, 2);
    assert_eq!(recur.position.declared_iterations, 5);
    // Element recur consumes exactly one element per iteration.
    assert_eq!(recur.position.consumed_elements, 1);
}

/// A tile that *can* break reports which branch it actually took.
#[test]
fn recur_tile_reports_break_and_continue_distinctly() {
    // `limit = 2`, `seen` starts at 0 -> first call continues, second breaks.
    let continues = raster::core::postcard::to_allocvec(&(
        RecurInput::new(String::from("a"), 0u64, 4u64),
        RecurState::new(LimitState { seen: 0 }),
        2u64,
    ))
    .unwrap();
    let breaks = raster::core::postcard::to_allocvec(&(
        RecurInput::new(String::from("b"), 1u64, 4u64),
        RecurState::new(LimitState { seen: 1 }),
        2u64,
    ))
    .unwrap();

    let continued =
        replay_journal(__raster_tile_replay_entry_count_until_limit_state_only(&continues).unwrap());
    let broke =
        replay_journal(__raster_tile_replay_entry_count_until_limit_state_only(&breaks).unwrap());

    assert_eq!(
        continued.recur.expect("recur facts").control,
        raster::core::draft::RecurControlKind::Continue,
    );
    assert_eq!(
        broke.recur.expect("recur facts").control,
        raster::core::draft::RecurControlKind::Break,
    );
}
