//! Whether a run authenticates the values it passes between tiles.
//!
//! The enum lives here, rather than next to the resolution logic in
//! `raster-runtime`, because `select!` expands to a match on it and must
//! compile in both the `std` host posture and the `no_std` guest posture.
//! Resolution is std-only and lives in `raster_runtime::auth`.
//!
//! See `docs/proposals/unauthenticated-execution.md` §1.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    /// Tile outputs go through storage: encoded, hashed, and passed on as a
    /// coordinate. What every posture in `01-runner-modes.md` assumes, and the
    /// only mode in which a trace — and therefore a trace commitment — exists.
    Authenticated,
    /// Tile outputs are passed directly as Rust values. Nothing is serialized,
    /// hashed, stored or resolved between tiles, and no trace is emitted.
    Unauthenticated,
}

impl AuthMode {
    pub fn is_authenticated(self) -> bool {
        matches!(self, Self::Authenticated)
    }
}
