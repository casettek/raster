//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile
//! - `#[sequence]` - Declares tile ordering and control flow

use proc_macro::TokenStream;

/// Marks a function as a Raster tile.
///
/// # Example
/// ```ignore
/// #[tile]
/// fn compute(input: u64) -> u64 {
///     input * 2
/// }
/// ```
#[proc_macro_attribute]
pub fn tile(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // TODO: Implement tile macro
    // - Register tile metadata
    // - Generate wrapper for tracing
    item
}

/// Declares a sequence of tiles with control flow.
///
/// # Example
/// ```ignore
/// #[sequence]
/// fn my_sequence() {
///     tile_a();
///     tile_b();
/// }
/// ```
#[proc_macro_attribute]
pub fn sequence(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // TODO: Implement sequence macro
    // - Parse control flow
    // - Generate schema
    item
}
