//! End-to-end coverage for `dynamic-index-selection`: a `select!` whose index
//! comes from an authorized value, all the way from the sequence body to the
//! recorded step the verifier sees.
//!
//! The unit tests in `raster-core` prove the verifier rejects every forged
//! citation; these prove an *honest* program produces a recording that satisfies
//! it — the direction that would otherwise only be exercised by running a full
//! proof. See `docs/proposals/dynamic-index-selection.md`.

use raster::core::trace::{verify_bound_index_bindings, TraceEvent};
use raster::prelude::*;
use raster::{IndexWidth, SelectorSegment};
use raster_runtime::{init_with, Publisher};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
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
        let capture_thread = *TRACE_CAPTURE_THREAD.lock().unwrap();
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

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Selectable)]
struct Row {
    token_id: u32,
    value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Table {
    /// The index into `rows`, itself a committed value — the whole point is
    /// that this travels with the same authorization as the rows do.
    wanted: u32,
    rows: List<Row>,
}

/// Commit a table to storage, as a tile output or `main` entry argument would
/// be, and hand back a binding to it. `select!` needs a committed source — an
/// inline sequence value has no root to prove anything against.
fn store_and_bind(table: Table) -> TypedStorageBinding<Table> {
    let reference = raster::store_value(&table).expect("store table");
    raster::typed_storage::<Table>(reference)
}

#[tile(kind = iter)]
fn emit_value(row: Row) -> String {
    row.value
}

/// The shape the proposal's appendix describes: gather one row by an authorized
/// index instead of scanning the table for it.
#[sequence]
fn gather_by_index(table: Table) -> String {
    let wanted = select!(u32, table.clone().wanted);
    let row = select!(Row, table.rows[wanted]);
    call!(emit_value, row)
}

fn run_gather(table: Table) -> String {
    let _guard = raster::__private::SequenceScopeGuard::enter("run_gather");
    materialize_auth_return::<String, _>(__raster_sequence_auth_gather_by_index(store_and_bind(
        table,
    )))
}

fn table() -> Table {
    Table {
        wanted: 2,
        rows: vec![
            Row {
                token_id: 10,
                value: "zero".to_string(),
            },
            Row {
                token_id: 11,
                value: "one".to_string(),
            },
            Row {
                token_id: 12,
                value: "two".to_string(),
            },
            Row {
                token_id: 13,
                value: "three".to_string(),
            },
        ]
        .into(),
    }
}

/// The tile receives the element at the index the committed value names — not
/// the element at some index a prover picked.
#[test]
fn dynamic_index_gathers_the_named_element() {
    let (value, _events) = capture_trace_events(|| run_gather(table()));
    assert_eq!(value, "two");
}

/// The recorded step must carry both bindings — the row *and* the value that
/// authorized its index — and must satisfy the verifier's obligations. A
/// recording that omitted the citation would be rejected as `MissingSource`, so
/// this is the test that the side-binding plumbing actually reaches the step.
#[test]
fn dynamic_index_records_a_verifiable_citation() {
    let (_value, events) = capture_trace_events(|| run_gather(table()));

    let tile_input = events
        .iter()
        .find_map(|event| match event {
            TraceEvent::TileExec(record) if record.fn_name == "emit_value" => record.input.as_ref(),
            _ => None,
        })
        .expect("expected an emit_value tile step");

    // The row binding is named for the tile parameter; the index binding is
    // content-named, so it cannot collide with any parameter.
    let (row_name, row) = tile_input
        .storage()
        .iter()
        .find(|(_, data)| {
            data.selection
                .path
                .segments
                .iter()
                .any(|segment| matches!(segment, SelectorSegment::BoundIndex { .. }))
        })
        .expect("expected a storage binding reached through a bound index");
    assert_eq!(row_name, "row");

    let SelectorSegment::BoundIndex {
        index,
        source,
        width,
    } = row
        .selection
        .path
        .segments
        .iter()
        .find(|segment| matches!(segment, SelectorSegment::BoundIndex { .. }))
        .expect("bound index segment")
    else {
        unreachable!()
    };

    assert_eq!(*index, 2, "the index used must be the committed `wanted`");
    assert_eq!(*width, IndexWidth::U32);
    assert!(
        source.starts_with("@idx/"),
        "index bindings are content-named under a prefix no Rust parameter can take, got {source}",
    );
    assert!(
        tile_input.storage().contains_key(source.as_str()),
        "the cited index binding must be recorded on the same step",
    );

    // The obligation the guest discharges, run over the real recording.
    assert_eq!(verify_bound_index_bindings(tile_input.storage()), Ok(()));
}

/// Two selections citing one authorized value collapse to a single binding.
/// That is what makes "the same index" *mean* the same index rather than two
/// independently forgeable ones.
#[sequence]
fn gather_twice(table: Table) -> String {
    let wanted = select!(u32, table.clone().wanted);
    let first = select!(Row, table.clone().rows[wanted]);
    let second = select!(Row, table.rows[wanted]);
    let _ = call!(emit_value, first);
    call!(emit_value, second)
}

#[test]
fn repeated_index_shares_one_binding() {
    let (value, events) = capture_trace_events(|| {
        let _guard = raster::__private::SequenceScopeGuard::enter("gather_twice");
        materialize_auth_return::<String, _>(__raster_sequence_auth_gather_twice(store_and_bind(
            table(),
        )))
    });
    assert_eq!(value, "two");

    for event in &events {
        let TraceEvent::TileExec(record) = event else {
            continue;
        };
        if record.fn_name != "emit_value" {
            continue;
        }
        let input = record.input.as_ref().expect("tile input");
        let cited: Vec<&String> = input
            .storage()
            .keys()
            .filter(|name| name.starts_with("@idx/"))
            .collect();
        assert_eq!(
            cited.len(),
            1,
            "one authorized index should yield exactly one binding, got {cited:?}",
        );
        assert_eq!(verify_bound_index_bindings(input.storage()), Ok(()));
    }
}

/// A literal index must keep emitting a plain `Index` segment and cite nothing —
/// existing programs are unaffected by this feature.
#[sequence]
fn gather_by_literal(table: Table) -> String {
    let row = select!(Row, table.rows[1]);
    call!(emit_value, row)
}

#[test]
fn literal_index_records_no_citation() {
    let (value, events) = capture_trace_events(|| {
        let _guard = raster::__private::SequenceScopeGuard::enter("gather_by_literal");
        materialize_auth_return::<String, _>(__raster_sequence_auth_gather_by_literal(
            store_and_bind(table()),
        ))
    });
    assert_eq!(value, "one");

    let tile_input = events
        .iter()
        .find_map(|event| match event {
            TraceEvent::TileExec(record) if record.fn_name == "emit_value" => record.input.as_ref(),
            _ => None,
        })
        .expect("expected an emit_value tile step");

    assert!(
        tile_input
            .storage()
            .keys()
            .all(|name| !name.starts_with("@idx/")),
        "a literal-index selection must not record an index binding",
    );
    let row = tile_input.storage().get("row").expect("row binding");
    assert!(
        row.selection
            .path
            .segments
            .iter()
            .any(|segment| matches!(segment, SelectorSegment::Index(1))),
        "expected a plain literal Index segment",
    );
    assert_eq!(verify_bound_index_bindings(tile_input.storage()), Ok(()));
}

// ---------------------------------------------------------------------------
// The motivating shape: a recur sequence that gathers one row per item, using
// the item itself as the index. This is `input-embedding`'s pass with the scan
// removed (proposal appendix), and the case dynamic indexes exist for.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Selectable)]
struct Gathered {
    values: List<String>,
}

#[tile(kind = iter)]
fn init_gathered(output: Draft<Gathered>) -> Draft<Gathered> {
    output
}

#[tile(kind = iter)]
fn append_gathered(output: Draft<Gathered>, row: Row) -> Draft<Gathered> {
    let mut output = output;
    output.values().push(row.value);
    output
}

/// One authorized row per item, by index — no scan, no fold state carrying a
/// matched row. `into_ref!` yields the item's `AuthRef` without materializing
/// it, which is what lets the id go straight into the selector.
#[sequence(kind = recur)]
fn gather_each(
    input: RecurSequenceInput<u32>,
    output: RecurSequenceOutput<Gathered>,
    rows: List<Row>,
) -> RecurSequenceOutput<Gathered> {
    let wanted = into_ref!(input);
    let row = select!(Row, rows[wanted]);
    call!(append_gathered, output, row)
}

#[sequence]
fn gather_all() -> Gathered {
    let wanted_source = raster::store_value(&vec![3u32, 1, 2]).expect("index list stores");
    let row_source = raster::store_value(&table().rows).expect("row list stores");
    let output = call!(init_gathered, new!(Gathered));

    call_recur_seq!(
        sequence = gather_each,
        input = storage!(List<u32>, wanted_source),
        output = output,
        args = (storage!(List<Row>, row_source),)
    )
}

fn run_gather_all() -> Gathered {
    materialize_auth_return::<Gathered, _>(__raster_sequence_auth_gather_all())
}

/// Each iteration reads the row its own committed item names, in item order.
#[test]
fn recur_sequence_gathers_by_authorized_index() {
    let (result, events) = capture_trace_events(run_gather_all);

    assert_eq!(
        result.values.into_vec(),
        vec!["three".to_string(), "one".to_string(), "two".to_string()],
        "each iteration must gather the row its own item indexes",
    );

    // Every gathering step must satisfy the verifier: the item's own binding is
    // cited as the index source, and it travels to the step alongside the row.
    let mut checked = 0;
    for event in &events {
        let TraceEvent::TileExec(record) = event else {
            continue;
        };
        if record.fn_name != "append_gathered" {
            continue;
        }
        let input = record.input.as_ref().expect("tile input");
        assert_eq!(verify_bound_index_bindings(input.storage()), Ok(()));
        assert!(
            input.storage().keys().any(|name| name.starts_with("@idx/")),
            "the gathering step must record the index binding it cites",
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "expected one gathering step per item");
}
