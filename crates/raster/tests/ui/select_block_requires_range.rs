use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Doc {
    lines: List<String>,
}

// `Block<T>` may only be produced by a literal range selection `xs[a..b]`.
// Naming a `Block` target for a whole-collection reference is rejected: the
// whole collection is a `List<T>`, which never crosses a tile boundary.
#[sequence]
fn main(doc: Doc) {
    let _bad = select!(Block<String>, doc.lines);
}
