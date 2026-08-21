use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Model {
    #[page_size = 4]
    weights: Bytes<4>,
}

#[tile]
fn bad(_model: Model) -> u64 {
    0
}

#[sequence]
fn main() {}
