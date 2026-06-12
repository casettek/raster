use raster::into_auth_value;
use raster::materialize_auth_return;
use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct LineBundle {
    title: String,
    items: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct SearchBundle {
    needle: String,
    matches: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct LimitedBundle {
    limit: u64,
    stopped_after: u64,
    items: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LimitState {
    seen: u64,
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

#[sequence]
fn build_lines_reference() -> InternalRef {
    let source = raster::store_internal_value(&vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = collect_lines,
        input = internal!(Vec<String>, source),
        output = new!(LineBundle),
        args = ()
    )
    .reference()
    .clone()
}

fn run_build_lines_reference() -> InternalRef {
    materialize_auth_return::<InternalRef, _>(__raster_sequence_auth_build_lines_reference())
}

#[sequence]
fn find_first_match(needle: String) -> SearchBundle {
    let source = raster::store_internal_value(&vec![
        "alpha".to_string(),
        "beta".to_string(),
        "gamma".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = collect_first_match,
        input = internal!(Vec<String>, source),
        output = new!(SearchBundle),
        args = (needle,)
    )
}

fn run_find_first_match(needle: String) -> SearchBundle {
    materialize_auth_return::<SearchBundle, _>(__raster_sequence_auth_find_first_match(needle))
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

#[sequence]
fn collect_two_items(limit: u64) -> LimitedBundle {
    let source = raster::store_internal_value(&vec![
        "one".to_string(),
        "two".to_string(),
        "three".to_string(),
    ])
    .expect("list source should store");

    call_recur!(
        tile = collect_until_limit,
        input = internal!(Vec<String>, source),
        state = LimitState { seen: 0 },
        output = new!(LimitedBundle),
        args = (limit,)
    )
}

fn run_collect_two_items(limit: u64) -> LimitedBundle {
    materialize_auth_return::<LimitedBundle, _>(__raster_sequence_auth_collect_two_items(limit))
}

#[test]
fn call_recur_finalizes_to_selectable_internal_ref() {
    let reference = run_build_lines_reference();

    let title = select!(String, internal!(LineBundle, reference.clone()).title);
    let first = select!(String, internal!(LineBundle, reference.clone()).items[0]);
    let third = select!(String, internal!(LineBundle, reference).items[2]);

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
    let auth = into_auth_ref::<LineBundle, _>(internal!(LineBundle, reference));

    let rendered = format!("{auth:?}");

    assert!(rendered.contains("AuthRef"));
    assert!(rendered.contains("storage: \"internal\""));
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
    assert_eq!(result.matches, vec!["beta".to_string()]);
}

#[test]
fn call_recur_threads_state_and_finalizes() {
    let result = run_collect_two_items(2);

    assert_eq!(result.limit, 2);
    assert_eq!(result.stopped_after, 2);
    assert_eq!(result.items, vec!["one".to_string(), "two".to_string()]);
}
