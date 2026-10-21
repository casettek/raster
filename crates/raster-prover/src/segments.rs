//! How many consecutive transition steps one segment guest receipt covers.
//!
//! Shared by the two segment guests, control flow
//! (`--control-flow-segment`) and storage (`--storage-segment`). Each is set
//! on its own, because their fixed costs differ: the control-flow guest
//! decodes the program and builds a `CfsCursor` per segment, the storage guest
//! reads no program. See `docs/proposals/control-flow-prover.md` §Segments and
//! `docs/proposals/storage-prover.md`.

use std::num::NonZeroUsize;
use std::str::FromStr;

/// How many consecutive steps one segment receipt covers.
///
/// A host-side choice only: the guests, the journal and the transition guest's
/// check are the same in every mode, so a relying party cannot tell — and need
/// not care — which one produced a proof.
///
/// The default is the whole window. Every segment pays its guest's fixed work
/// and at least one segment of proving, while a transition step's own cost is
/// one entry and a logarithmic path whatever the segment's length. Measured on
/// `hello-tiles` and `prompt-prepare` for control flow (dev mode, 2026-10-21),
/// the whole window came out cheapest or tied. A fixed length still bounds a
/// single segment guest execution on very long windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SegmentLength {
    /// One receipt for the whole window: the guest's fixed work once.
    #[default]
    Window,
    /// One receipt per `n` steps, the last segment possibly shorter. `Steps(1)`
    /// is the per-step mode; a larger `n` bounds a segment's size on long
    /// windows.
    Steps(NonZeroUsize),
}

impl SegmentLength {
    /// `0` is the whole window; any other `n` is `n` steps per segment.
    pub fn from_len(len: usize) -> Self {
        NonZeroUsize::new(len).map_or(Self::Window, Self::Steps)
    }

    /// The segment length for a window of `window_len` steps.
    pub(crate) fn len_for(self, window_len: usize) -> usize {
        match self {
            Self::Window => window_len.max(1),
            Self::Steps(n) => n.get(),
        }
    }
}

impl FromStr for SegmentLength {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "window" => Ok(Self::Window),
            n => n
                .parse::<usize>()
                .map(Self::from_len)
                .map_err(|_| format!("expected `window` or a number of steps, got `{n}`")),
        }
    }
}

/// The segment length of each segment guest, set independently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentLengths {
    pub control_flow: SegmentLength,
    pub storage: SegmentLength,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_segment_length_parses_from_the_command_line() {
        assert_eq!("window".parse(), Ok(SegmentLength::Window));
        assert_eq!("0".parse(), Ok(SegmentLength::Window));
        assert_eq!(
            "1".parse(),
            Ok(SegmentLength::Steps(NonZeroUsize::new(1).unwrap()))
        );
        assert!("one".parse::<SegmentLength>().is_err());
        assert_eq!(SegmentLength::default(), SegmentLength::Window);
    }

    #[test]
    fn a_window_segment_covers_the_whole_window() {
        assert_eq!(SegmentLength::Window.len_for(32), 32);
        assert_eq!(SegmentLength::Window.len_for(0), 1);
        assert_eq!(SegmentLength::from_len(8).len_for(32), 8);
    }
}
