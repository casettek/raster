use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Selectable, Merklized)]
struct PersonalData {
    name: String,
}

fn takes_untyped_binding(_: SelectedExternalBinding) {}

fn takes_typed_binding(_: TypedSelectedExternalBinding<PersonalData>) {}

#[test]
fn select_accepts_identity_untyped_external() {
    takes_untyped_binding(select!(external!("seed")));
}

#[test]
fn select_accepts_identity_typed_external() {
    takes_typed_binding(select!(external!(PersonalData, "personal_data")));
}
