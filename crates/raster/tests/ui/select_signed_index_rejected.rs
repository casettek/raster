use raster::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Selectable)]
struct Doc {
    lines: List<String>,
    offset: i32,
}

// `IndexSource` is implemented only for `AuthRef<uN>`. A signed index is
// therefore a type error, not a runtime one: there is no meaning for a negative
// list index, and accepting one would push the check to the verifier, where a
// truncating conversion is a forgery (see `IndexWidth::encode`).
#[sequence]
fn main(doc: Doc) {
    let offset = select!(i32, doc.clone().offset);
    let _bad = select!(String, doc.lines[offset]);
}
