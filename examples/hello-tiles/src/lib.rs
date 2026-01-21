//! Hello Tiles example library.
//!
//! This module exports the tile functions so they can be used by both
//! the binary and RISC0 guest programs.
//!
//! This library is `no_std` compatible for use in RISC0 guests.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use raster::prelude::*;

/// A simple tile that greets a user by name.
///
/// This tile takes a String input and returns a greeting.
#[tile]
pub fn greet(name: String) -> String {
    format!("Hello, {}!!", name)
}

/// A tile that adds emphasis to a message.
///
/// This tile takes a String and returns it with exclamation marks.
#[tile]
pub fn exclaim(message: String) -> String {
    format!("{}!!", message)
}

#[tile]
pub fn raster_wish(message: String) -> String {
    format!("{}\nHope you  will have fun with Raster!", message)
}

#[tile]
pub fn current_wish(message: String) -> String {
    format!("{}\nHappy new year!", message)
}

/// A tile that computes Fibonacci numbers.
///
/// This demonstrates a more computationally intensive tile.
#[tile]
pub fn fibonacci(n: u64) -> u64 {
    if n <= 1 {
        return n;
    }
    let mut a = 0u64;
    let mut b = 1u64;
    for _ in 2..=n {
        let c = a.wrapping_add(b);
        a = b;
        b = c;
    }
    b
}
