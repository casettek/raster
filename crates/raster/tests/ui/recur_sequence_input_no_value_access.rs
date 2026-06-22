use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
struct Doc {
    title: String,
}

#[sequence(kind = recur)]
fn invalid(
    input: RecurSequenceInput<String>,
    output: RecurSequenceOutput<Doc>,
) -> RecurSequenceOutput<Doc> {
    let _value = input.into_value();
    output
}

fn main() {}
