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
use serde::{Deserialize, Serialize};

use crate::input::{CollectiveGreeting, CollectiveGreetingDraftExt, PersonalData};

pub mod input;

/// A simple tile that greets a user by name.
///
/// This tile takes a String input and returns a greeting.
#[tile(kind = iter)]
pub fn greet(name: String) -> String {
    format!("Hello, {}!!!!", name)
}

#[tile(kind=iter)]
pub fn personal_greet(name: String) -> String {
    let greet = format!("Hello, {}!!!!", name);
    debug!("greet: {}", greet);

    greet
}

#[tile(kind=iter)]
pub fn personal_greet_from_object(personal_data: PersonalData) -> String {
    let greet = format!("Hello from object, {}!!!!", personal_data.name);
    debug!("object greet: {}", greet);

    greet
}

/// A greeting tile that resolves both committed postcard-encoded example inputs:
/// schema-selected `personal_data.name` and `seed`.
#[tile(kind=iter)]
pub fn personal_greet_with_seed(name: String, seed: u64) -> String {
    let greet = format!("Hello, {}!!!! (seed: {})", name, seed);
    debug!("seeded greet: {}", greet);

    greet
}

#[tile(kind=iter)]
pub fn greet_address_line(address_line: String) -> String {
    let greet = format!("Address line: {}", address_line);
    debug!("address line: {}", greet);

    greet
}

#[tile(kind = iter)]
pub fn maybe_echo_name(name: String) -> Result<String> {
    if name.is_empty() {
        Err(String::from("MissingName"))
    } else {
        Ok(name)
    }
}

/// A tile that adds emphasis to a message.
///
/// This tile takes a String and returns it with exclamation marks.
#[tile(kind = iter)]
pub fn exclaim(message: String) -> String {
    format!("{}!!!!", message)
}

#[tile(kind = iter)]
pub fn concat_messages(message1: String, message2: String) -> String {
    format!("{} {}", message1, message2)
}

#[tile(kind = iter)]
pub fn set_draft_greeting_title(
    title: String,
    draft: Draft<CollectiveGreeting>,
) -> Draft<CollectiveGreeting> {
    let mut draft = draft;
    draft.title().set(title);
    draft
}

#[tile(kind = iter)]
pub fn push_draft_greeting_line(
    line: String,
    draft: Draft<CollectiveGreeting>,
) -> Draft<CollectiveGreeting> {
    let mut draft = draft;
    draft.lines().push(line);
    draft
}

#[tile(kind = recur)]
pub fn build_recur_draft_greeting(
    input: RecurInput<String>,
    output: RecurOutput<CollectiveGreeting>,
    title: String,
) -> RecurOutput<CollectiveGreeting> {
    let mut output = output;
    if input.is_first() {
        output.title().set(title);
    }
    output.lines().push(input.into_value());
    output
}

/// State returned from a state-only recur tile.
#[derive(Clone, Debug, Deserialize, Serialize, Selectable)]
pub struct LineLengthStats {
    pub max_len: u64,
}

/// Internal state used by a state+output recur tile.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GreetingLimitState {
    pub seen: u64,
}

/// State-only recur: reduce a list of lines down to a single summary value.
#[tile(kind = recur)]
pub fn compute_recur_max_line_len(
    input: RecurInput<String>,
    state: RecurState<LineLengthStats>,
) -> RecurState<LineLengthStats> {
    let mut state = state;
    let len = input.value().len() as u64;
    if len > state.max_len {
        state.max_len = len;
    }
    state
}

/// State+output recur: use loop-carried state to stop building output early.
#[tile(kind = recur)]
pub fn build_limited_recur_greeting(
    input: RecurInput<String>,
    state: RecurState<GreetingLimitState>,
    output: RecurOutput<CollectiveGreeting>,
    title: String,
    limit: u64,
) -> RecurControl<(
    RecurState<GreetingLimitState>,
    RecurOutput<CollectiveGreeting>,
)> {
    let mut state = state;
    let mut output = output;
    if input.is_first() {
        output.title().set(title);
    }

    state.seen += 1;
    output.lines().push(input.into_value());

    if state.seen >= limit {
        RecurControl::Break((state, output))
    } else {
        RecurControl::Continue((state, output))
    }
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
#[tile(kind = iter)]
pub fn count_to(current: u64, goal: u64) -> (bool, u64, u64) {
    if current >= goal {
        // Goal reached, we're done
        (true, current, goal)
    } else {
        // Keep counting: increment current, pass goal through
        (false, current + 1, goal)
    }
}
