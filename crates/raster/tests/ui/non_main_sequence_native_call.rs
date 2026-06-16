use raster::prelude::*;

#[tile]
fn echo(name: String) -> String {
    name
}

#[sequence]
fn child(name: String) -> String {
    call!(echo, name)
}

fn plain_rust() {
    let _ = child("Raster".to_string());
}

fn main() {}
