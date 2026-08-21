use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Model {
    #[page_size = 4]
    weights: Bytes<4>,
}

#[sequence]
fn main(model: Model) {
    let _bad = select!(BytesPage, model.weights);
}
