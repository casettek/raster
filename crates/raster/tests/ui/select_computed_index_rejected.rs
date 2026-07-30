use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Doc {
    lines: List<String>,
    cursor: u32,
}

// A `select!` index must be a literal, a `start..end` range, or a *binding* in
// scope. Anything computed in the sequence body has no lineage (SKILL.md §4), so
// an index derived from arithmetic is exactly the prover-choosable index that
// `BoundIndex` exists to rule out — the macro rejects it outright rather than
// recording an unauthenticated number as provenance.
#[sequence]
fn main(doc: Doc) {
    let cursor = select!(u32, doc.clone().cursor);
    let _bad = select!(String, doc.lines[cursor + 1]);
}
