use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Doc {
    title: String,
}

#[sequence(kind = recur)]
fn invalid(input: RecurInput<String>, output: RecurOutput<Doc>) -> RecurOutput<Doc> {
    let _ = input;
    output
}

fn main() {}
