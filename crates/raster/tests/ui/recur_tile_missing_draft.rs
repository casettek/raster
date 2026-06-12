use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Doc {
    title: String,
}

#[tile(kind = recur)]
fn invalid(input: RecurInput<String>) -> RecurOutput<Doc> {
    let _ = input;
    panic!("not reached")
}

fn main() {}
