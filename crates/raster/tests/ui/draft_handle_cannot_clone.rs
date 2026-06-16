use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Selectable)]
struct Account {
    balance: u64,
}

fn attempt_clone() {
    let draft = new!(Account);
    let _copy = draft.clone();
}

fn main() {}
