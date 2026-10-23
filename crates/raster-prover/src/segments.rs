//! How many consecutive window steps one part segment receipt covers, and the
//! other host-side proving options.
//!
//! Each of the four parts is set on its own: trace (`--trace-segment`),
//! control flow (`--control-flow-segment`), storage (`--storage-segment`) and
//! transition (`--transition-segment`), because their fixed costs differ: the
//! control-flow and transition parts decode the program per segment, the
//! storage and trace parts read none. FRAUD WINDOW stitches the segments
//! whatever their lengths. See `docs/proposals/transition-split.md`.

use std::num::NonZeroUsize;
use std::str::FromStr;

/// How many consecutive steps one segment receipt covers.
///
/// A host-side choice only: the guests and FRAUD WINDOW's checks are the same
/// in every mode, so a relying party cannot tell — and need not care — which
/// one produced a proof.
///
/// The default is the whole window: every segment pays its guest's fixed work
/// and at least one zkVM segment of proving, and FRAUD WINDOW one stitch.
/// Measured on one RTX 3070 (`transition-split` §Step 5 record), the whole
/// window is fastest: `4` costs about 1.3×, one step per segment 2–2.5×.
/// Shorter segments can pay where several provers with their own memory run
/// at once, and bound a single guest execution on very long windows.
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

/// The segment length of each of the four parts, set independently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentLengths {
    pub trace: SegmentLength,
    pub control_flow: SegmentLength,
    pub storage: SegmentLength,
    pub transition: SegmentLength,
}

/// The size of the zkVM's own proving segments: 2^po2 cycles each.
///
/// Not to be confused with [`SegmentLength`], which counts *window steps* per
/// receipt. This is risc0's continuation segment, the unit a prover holds in
/// memory at once. risc0 defaults to 2^20 cycles; a smaller one trades more
/// segments for less GPU memory. Measured 2026-10-22 on an 8 GB RTX 3070: the
/// window guests run out of memory at 2^20 and 2^19 and prove at 2^18
/// (`docs/proposals/transition-split.md` §GPU baseline). risc0 3.0.6 reads no
/// environment variable for it, so it is set on every executor environment the
/// prover builds ([`Self::apply`]). The receipt a relying party verifies is
/// the same whatever the size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZkvmSegmentPo2(Option<u32>);

impl ZkvmSegmentPo2 {
    /// The smallest and largest sizes accepted: risc0's own tests run 2^13;
    /// 2^22 is its `DEFAULT_MAX_PO2`.
    pub const MIN: u32 = 13;
    pub const MAX: u32 = 22;

    /// risc0's default.
    pub const fn default_size() -> Self {
        Self(None)
    }

    pub fn po2(self) -> Option<u32> {
        self.0
    }

    /// Set the segment size on an executor environment, if one was chosen.
    pub fn apply(self, builder: &mut risc0_zkvm::ExecutorEnvBuilder<'_>) {
        if let Some(po2) = self.0 {
            builder.segment_limit_po2(po2);
        }
    }

    /// A fresh executor environment builder with the segment size applied.
    pub fn env_builder<'a>(self) -> risc0_zkvm::ExecutorEnvBuilder<'a> {
        let mut builder = risc0_zkvm::ExecutorEnv::builder();
        self.apply(&mut builder);
        builder
    }
}

impl FromStr for ZkvmSegmentPo2 {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let po2: u32 = s
            .parse()
            .map_err(|_| format!("expected a power of two exponent, got `{s}`"))?;
        if !(Self::MIN..=Self::MAX).contains(&po2) {
            return Err(format!(
                "segment size 2^{po2} is outside 2^{}..=2^{}",
                Self::MIN,
                Self::MAX
            ));
        }
        Ok(Self(Some(po2)))
    }
}

/// Everything the host chooses about how a fraud window is proven. None of it
/// changes what the FRAUD WINDOW receipt says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProveOptions {
    /// How many window steps each part segment receipt covers.
    pub segments: SegmentLengths,
    /// The zkVM's proving segment size.
    pub zkvm_segment_po2: ZkvmSegmentPo2,
    /// How many independent proofs run at once (`schedule::run_jobs`).
    pub workers: crate::schedule::ProverWorkers,
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
    fn the_zkvm_segment_size_parses_within_bounds() {
        assert_eq!("18".parse::<ZkvmSegmentPo2>().unwrap().po2(), Some(18));
        assert_eq!(ZkvmSegmentPo2::default().po2(), None);
        assert!("12".parse::<ZkvmSegmentPo2>().is_err());
        assert!("23".parse::<ZkvmSegmentPo2>().is_err());
        assert!("big".parse::<ZkvmSegmentPo2>().is_err());
    }

    #[test]
    fn a_window_segment_covers_the_whole_window() {
        assert_eq!(SegmentLength::Window.len_for(32), 32);
        assert_eq!(SegmentLength::Window.len_for(0), 1);
        assert_eq!(SegmentLength::from_len(8).len_for(32), 8);
    }
}
