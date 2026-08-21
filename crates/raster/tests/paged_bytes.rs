use raster::materialize_auth_return;
use raster::prelude::*;
use raster_runtime::{init_with, Publisher};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::thread::ThreadId;

use raster::core::trace::TraceEvent;

static TRACE_CAPTURE_LOCK: Mutex<()> = Mutex::new(());
static TRACE_INIT: Once = Once::new();
static TRACE_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());
static TRACE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static TRACE_CAPTURE_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

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
struct Model {
    #[page_size = 4]
    weights: Bytes<4>,
}

fn store_model(bytes: Vec<u8>) -> StorageRef {
    let model = Model {
        weights: Bytes::<4>::paged(bytes).expect("page the fixture"),
    };
    raster::store_value(&model).expect("store model")
}

#[tile(kind = recur, description = "sum the bytes of one page")]
fn sum_page(input: RecurInput<BytesPage>, state: RecurState<u64>) -> RecurState<u64> {
    let mut state = state;
    *state += input
        .into_value()
        .as_slice()
        .iter()
        .map(|byte| *byte as u64)
        .sum::<u64>();
    state
}

#[tile(kind = recur, description = "sum the bytes of a page chunk")]
fn sum_page_chunk(input: RecurInput<Block<BytesPage>>, state: RecurState<u64>) -> RecurState<u64> {
    let mut state = state;
    for page in input.into_value().iter() {
        *state += page.as_slice().iter().map(|byte| *byte as u64).sum::<u64>();
    }
    state
}

#[sequence]
fn sweep_pages() -> u64 {
    let source = store_model(vec![1, 2, 3, 4, 5]);
    let pages = select!(List<BytesPage>, storage!(Model, source).weights.pages);
    call_recur!(
        tile = sum_page,
        input = pages,
        state = 0u64,
        args = ()
    )
}

#[sequence]
fn sweep_pages_chunked() -> u64 {
    let source = store_model(vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let pages = select!(List<BytesPage>, storage!(Model, source).weights.pages);
    call_recur!(
        tile = sum_page_chunk,
        input = pages,
        chunk = 2,
        state = 0u64,
        args = ()
    )
}

#[sequence]
fn sweep_in_callee(pages: List<BytesPage>) -> u64 {
    call_recur!(
        tile = sum_page,
        input = pages,
        state = 0u64,
        args = ()
    )
}

#[sequence]
fn sweep_across_boundary() -> u64 {
    let source = store_model(vec![1, 2, 3, 4, 5]);
    let pages = select!(List<BytesPage>, storage!(Model, source).weights.pages);
    call_seq!(sweep_in_callee, pages)
}

fn run_sweep_pages() -> u64 {
    materialize_auth_return::<u64, _>(__raster_sequence_auth_sweep_pages())
}

fn run_sweep_pages_chunked() -> u64 {
    materialize_auth_return::<u64, _>(__raster_sequence_auth_sweep_pages_chunked())
}

fn run_sweep_across_boundary() -> u64 {
    materialize_auth_return::<u64, _>(__raster_sequence_auth_sweep_across_boundary())
}

fn recur_iteration_count(events: &[TraceEvent], tile: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                TraceEvent::RecurTileIterationExec(record) if record.fn_name == tile
            )
        })
        .count()
}

#[test]
fn sweep_over_pages_visits_each_page_including_the_short_final() {
    let (sum, events) = capture_trace_events(run_sweep_pages);
    assert_eq!(sum, 15);
    assert_eq!(recur_iteration_count(&events, "sum_page"), 2);
}

#[test]
fn sweep_chunk_m_accepts_a_short_final_chunk() {
    let (sum, events) = capture_trace_events(run_sweep_pages_chunked);
    // 9 bytes / 4 → 3 pages; chunk = 2 → iterations of 2 then 1.
    assert_eq!(sum, 45);
    assert_eq!(recur_iteration_count(&events, "sum_page_chunk"), 2);
}

#[test]
fn sweep_across_a_sequence_boundary_matches_in_place() {
    let (in_place, in_place_events) = capture_trace_events(run_sweep_pages);
    let (across, across_events) = capture_trace_events(run_sweep_across_boundary);
    assert_eq!(in_place, across);
    assert_eq!(
        recur_iteration_count(&in_place_events, "sum_page"),
        recur_iteration_count(&across_events, "sum_page")
    );
}
