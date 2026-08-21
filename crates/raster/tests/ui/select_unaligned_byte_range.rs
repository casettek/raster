use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Model {
    #[page_size = 4]
    weights: Bytes<4>,
}

// `select!` emits `page_range_for_*::<…, START, END>` whose `const { assert! }`
// fires when rustc monomorphizes (`cargo build`). trybuild type-checks only
// (`cargo check`), so the same alignment predicate is spelled here against
// the field's `Bytes<P>` — the type `select!` cannot name, but the one that
// owns `PAGE_SIZE`.
const _: () = assert!(
    1u64 % <Bytes<4> as PageSized>::PAGE_SIZE == 0
        && 5u64 % <Bytes<4> as PageSized>::PAGE_SIZE == 0,
    "select! byte range is not page-aligned"
);

#[sequence]
fn main(model: Model) {
    let _bad = select!(Block<BytesPage>, model.weights[1..5]);
}
