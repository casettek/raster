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
#[tile(kind = iter)]
pub fn greet(name: String) -> String {
    format!("Hello, {}!!!!", name)
}

/// A tile that adds emphasis to a message.
///
/// This tile takes a String and returns it with exclamation marks.
#[tile(kind = iter)]
pub fn exclaim(message: String) -> String {
    format!("{}!!!!", message)
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
#[tile(kind = iter)]
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

/// A recursive tile that increments a value until it reaches a goal.
///
/// This demonstrates a tail-recursive pattern where:
/// - The first output (`done`) indicates if the recursion is complete
/// - The remaining outputs (`current`, `goal`) become inputs for the next iteration
///
/// Example: count_to(0, 3) -> (false, 1, 3)  
///          count_to(1, 3) -> (false, 2, 3)  
///          count_to(2, 3) -> (false, 3, 3)  
///          count_to(3, 3) -> (true, 3, 3)   <- done! reached the goal
#[tile(kind = recur)]
pub fn count_to(current: u64, goal: u64) -> (bool, u64, u64) {
    if current >= goal {
        // Goal reached, we're done
        (true, current, goal)
    } else {
        // Keep counting: increment current, pass goal through
        (false, current + 1, goal)
    }
}
