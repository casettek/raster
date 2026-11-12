//! A tile's input `data` is encoded after the call, from clones taken before it
//! (`raster_runtime::tracing::deferred_input`). Only arguments whose clone is an
//! independent snapshot may be deferred: a clone that shares writable state
//! with the argument would let the tile change what the trace records as its
//! input.

use raster::core::input::SchemaNode;
use raster::core::trace::{FnInput, FnInputValue, TraceEvent};
use raster::prelude::*;
use raster::{Block, BytesPage, Materializable, SnapshotClone};
use raster_runtime::{init_with, Publisher};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::ThreadId;

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

fn tile_input<'a>(events: &'a [TraceEvent], tile: &str) -> &'a FnInput {
    events
        .iter()
        .find_map(|event| match event {
            TraceEvent::TileExec(record) if record.fn_name == tile => record.input.as_ref(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no traced `{tile}` call"))
}

/// A hand-written `Materializable` whose clones share one counter: `Clone` is
/// no snapshot, so it must not get [`SnapshotClone`].
#[derive(Clone, Debug)]
struct Counter(Arc<Mutex<u64>>);

impl Counter {
    fn new(value: u64) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }
}

impl Serialize for Counter {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.lock().unwrap().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Counter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        u64::deserialize(deserializer).map(Counter::new)
    }
}

impl Materializable for Counter {}

impl Selectable for Counter {
    fn schema() -> SchemaNode {
        u64::schema()
    }
}

/// Derived around a field without the promise, so it has none either: the
/// derive's impl must not apply, and the struct must still compile.
#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Holder {
    counter: Counter,
    tag: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Owned {
    tag: u32,
    name: String,
    window: Block<u8>,
}

#[tile]
fn bump(counter: Counter) -> u64 {
    std::mem::replace(&mut *counter.0.lock().unwrap(), 99)
}

#[tile]
fn bump_held(holder: Holder) -> u64 {
    std::mem::replace(&mut *holder.counter.0.lock().unwrap(), 99) + u64::from(holder.tag)
}

#[test]
fn a_shared_argument_is_recorded_as_the_tile_received_it() {
    let counter = Counter::new(7);
    let (seen, events) = capture_trace_events(|| {
        let _scope = raster::__private::SequenceScopeGuard::enter("bump_shared");
        bump(counter.clone())
    });
    assert_eq!(seen, 7);
    assert_eq!(*counter.0.lock().unwrap(), 99, "the tile changed the shared value");

    let input = tile_input(&events, "bump");
    let received = raster::core::postcard::to_allocvec(&7u64).unwrap();
    assert_eq!(input.data, received, "`data` must hold the value the tile received");
    assert!(matches!(&input.values[..], [FnInputValue::Inline(bytes)] if *bytes == received));
}

#[test]
fn a_struct_with_a_shared_field_is_recorded_as_the_tile_received_it() {
    let holder = Holder {
        counter: Counter::new(7),
        tag: 3,
    };
    let (seen, events) = capture_trace_events(|| {
        let _scope = raster::__private::SequenceScopeGuard::enter("bump_held_shared");
        bump_held(holder.clone())
    });
    assert_eq!(seen, 10);
    assert_eq!(*holder.counter.0.lock().unwrap(), 99);

    let input = tile_input(&events, "bump_held");
    let received = raster::core::postcard::to_allocvec(&(7u64, 3u32)).unwrap();
    assert_eq!(input.data, received);
}

#[test]
fn owned_values_keep_the_snapshot_promise() {
    fn snapshot<T: SnapshotClone>() {}
    snapshot::<u64>();
    snapshot::<String>();
    snapshot::<Option<[u8; 4]>>();
    snapshot::<(u32, Block<u16>)>();
    snapshot::<BytesPage>();
    snapshot::<Block<BytesPage>>();
    snapshot::<Owned>();
    snapshot::<Block<Owned>>();
    snapshot::<raster::RecurInput<BytesPage>>();
    snapshot::<raster::RecurState<u64>>();
}
