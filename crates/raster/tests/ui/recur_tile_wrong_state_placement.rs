use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Doc {
    title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScanState {
    seen: u64,
}

#[tile(kind = recur)]
fn invalid(
    input: RecurInput<String>,
    output: RecurOutput<Doc>,
    state: RecurState<ScanState>,
) -> (RecurState<ScanState>, RecurOutput<Doc>) {
    let _ = input;
    let _ = output;
    let _ = state;
    panic!("not reached")
}

fn main() {}
