//! Whether a run authenticates the values it passes between tiles.
//!
//! See `docs/proposals/unauthenticated-execution.md` §1. The short version:
//! authenticated storage costs a serialize, a hash, a store, a load and a
//! deserialize per inter-tile value, and the only readers of that work are the
//! trace (`FnInput.storage`) and `--commit`/`--audit` on top of it. A run that
//! writes no trace pays for all of it and nothing reads any of it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

pub use raster_core::auth::AuthMode;

/// Selects `1`/`on` (authenticated) or `0`/`off` (unauthenticated) explicitly,
/// overriding the entry-point default either way. `cargo raster run` sets it.
pub const AUTH_ENV: &str = "RASTER_AUTH";

static MODE: OnceLock<AuthMode> = OnceLock::new();

/// Set only by [`crate::tracing::init`], which the `#[sequence]` macro calls
/// from the `fn main` it generates for a program entry point — and from nowhere
/// else in the codebase. That is what distinguishes "a Raster program was
/// launched" from "something is driving the runtime directly", and it is why
/// `cargo test` is authenticated without any test opting in: no test reaches
/// `init`.
static PROGRAM_ENTRY: AtomicBool = AtomicBool::new(false);

/// Record that this process is a Raster program run, lowering the default to
/// [`AuthMode::Unauthenticated`].
///
/// MUST be called before the first [`auth_mode`], or the mode caches as
/// `Authenticated` and the program silently runs the expensive way.
pub fn note_program_entry() {
    PROGRAM_ENTRY.store(true, Ordering::SeqCst);
}

/// The mode for this process, resolved once and then fixed.
///
/// Resolution order:
/// 1. `RASTER_AUTH`, if set — always wins;
/// 2. `Unauthenticated`, if this is a program entry point ([`note_program_entry`]);
/// 3. `Authenticated`.
///
/// Caching is load-bearing rather than an optimization: a mode that could read
/// as two different values within one run would let a sequence store half its
/// bindings and pass the rest directly, producing a trace that is neither one
/// thing nor the other.
pub fn auth_mode() -> AuthMode {
    *MODE.get_or_init(resolve)
}

/// Pin the mode explicitly, before anything reads it.
///
/// For tests and embedders that drive the runtime directly and want a mode
/// other than the [`AuthMode::Authenticated`] default. Returns `Err` with the
/// already-resolved mode if [`auth_mode`] has run — the mode is fixed for the
/// life of the process by design, so this cannot change it after the fact.
pub fn force_auth_mode(mode: AuthMode) -> Result<(), AuthMode> {
    MODE.set(mode).map_err(|_| auth_mode())
}

fn resolve() -> AuthMode {
    if let Some(raw) = std::env::var_os(AUTH_ENV) {
        let value = raw
            .to_str()
            .unwrap_or_else(|| panic!("{AUTH_ENV} must be valid UTF-8"));
        return match value.trim().to_ascii_lowercase().as_str() {
            "1" | "on" | "true" | "yes" => AuthMode::Authenticated,
            "0" | "off" | "false" | "no" => AuthMode::Unauthenticated,
            other => panic!(
                "{AUTH_ENV} must be one of 1/on/true/yes or 0/off/false/no, got '{other}'"
            ),
        };
    }

    if PROGRAM_ENTRY.load(Ordering::SeqCst) {
        AuthMode::Unauthenticated
    } else {
        AuthMode::Authenticated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_is_the_default_without_a_program_entry() {
        // The property `cargo test` depends on: a binary that never reaches
        // `init` authenticates, so tests written to check authentication
        // actually check it. Skipped when the env var is set, which overrides
        // the default this asserts.
        if std::env::var_os(AUTH_ENV).is_some() {
            return;
        }
        assert!(!PROGRAM_ENTRY.load(Ordering::SeqCst));
        assert_eq!(resolve(), AuthMode::Authenticated);
    }

    #[test]
    fn is_authenticated_matches_the_variant() {
        assert!(AuthMode::Authenticated.is_authenticated());
        assert!(!AuthMode::Unauthenticated.is_authenticated());
    }
}
