use raster::prelude::*;

#[tile]
fn greet(name: String) -> String {
    format!("Hello, {}!", name)
}

#[tile]
fn exclaim(message: String) -> String {
    format!("{}!!!", message)
}

#[sequence]
fn hello_sequence() {
    let greeting = greet("Raster".to_string());
    let excited = exclaim(greeting);
    println!("{}", excited);
}

fn main() {
    println!("Hello Tiles Example");
    // TODO: Execute the sequence
}
