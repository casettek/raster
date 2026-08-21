use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Model {
    #[page_size = 4]
    weights: Bytes<4>,
}

#[tile(kind = recur, description = "accumulate a page")]
fn accumulate(input: RecurInput<BytesPage>, state: RecurState<u64>) -> RecurState<u64> {
    let _ = input.into_value();
    state
}

#[sequence]
fn main(model: Model) {
    let weights = select!(Bytes<4>, model.weights);
    call_recur!(
        tile = accumulate,
        input = weights,
        state = 0u64,
        args = ()
    );
}
