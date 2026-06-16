use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Doc {
    title: String,
}

#[tile(kind = recur)]
fn invalid(input: RecurInput<String>, output: RecurOutput<Doc>) -> bool {
    let _ = input;
    let _ = output;
    false
}

fn main() {}
