use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Selectable)]
struct Account {
    balance: u64,
}

fn consume(_: Draft<Account>) {}

fn attempt_reuse() {
    let draft = new!(Account);
    consume(draft);
    consume(draft);
}

fn main() {}
