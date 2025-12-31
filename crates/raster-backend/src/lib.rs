//! Backend abstraction layer for the Raster toolchain.
//!
//! This crate defines the `Backend` trait that all compilation and execution
//! backends must implement. It also provides the native backend implementation.

pub mod backend;
pub mod native;

pub use backend::Backend;
pub use native::NativeBackend;
