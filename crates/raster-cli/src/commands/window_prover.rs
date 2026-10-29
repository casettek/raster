//! One API for proving a window of a trace against a named commitment.
//!
//! Every caller of the proving pipeline needs the same operation — *prove
//! window W of trace T against commitment C, and say how it ended* — and must
//! not mistake one ending for the other: a fraud proof (`Finished`) and an
//! attestation of an honest end (`TerminalMatch`) are different claims. The
//! outcome is therefore read from the journal the guest committed, and each
//! method refuses the ending it did not ask for. See
//! `docs/proposals/stage-challenge.md` §Phase 2.

use raster_core::cfs::ControlFlowSchema;
use raster_core::merkle::EMPTY_LEAF;
use raster_core::trace::Trace;
use raster_core::transition::{TransitionJournal, TransitionState};
use raster_core::{Error, Result};
use raster_prover::replay::Replayer;
use raster_prover::trace::{FraudEvidence, TraceCommitment, TraceCommitmentExt, TraceVerifier};
use raster_runtime::TraceRecorder;

/// How a proven window ended — read from the journal, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowOutcome {
    /// The last item diverged: a fraud proof against the commitment.
    Diverged,
    /// A terminal window that matched throughout: an attestation of the
    /// trace's honest end.
    TerminalMatch,
}

impl WindowOutcome {
    /// The outcome a journal's final state records, if it records one: a
    /// window still in `Init` or `Next` has not ended.
    pub fn of(state: &TransitionState) -> Option<Self> {
        match state {
            TransitionState::Finished => Some(Self::Diverged),
            TransitionState::TerminalMatch => Some(Self::TerminalMatch),
            TransitionState::Init(_) | TransitionState::Next(_) => None,
        }
    }
}

/// A proven window: the receipt, its decoded journal, and how it ended.
pub struct WindowProof {
    pub receipt: risc0_zkvm::Receipt,
    pub journal: TransitionJournal,
    pub outcome: WindowOutcome,
}

/// Proves windows of one trace against one named commitment.
pub struct WindowProver<'a> {
    trace: &'a Trace,
    commitment: &'a TraceCommitment,
    cfs: &'a ControlFlowSchema,
    recorder: &'a TraceRecorder,
    replayer: &'a Replayer<'a>,
    input_manifest: Option<&'a str>,
}

impl<'a> WindowProver<'a> {
    pub fn new(
        trace: &'a Trace,
        commitment: &'a TraceCommitment,
        cfs: &'a ControlFlowSchema,
        recorder: &'a TraceRecorder,
        replayer: &'a Replayer<'a>,
        input_manifest: Option<&'a str>,
    ) -> Self {
        Self {
            trace,
            commitment,
            cfs,
            recorder,
            replayer,
            input_manifest,
        }
    }

    /// The fraud window a `TraceVerifier::verify` divergence names: a fraud
    /// proof. Errors unless the guest ended it `Finished`.
    pub fn prove_divergence(&self, evidence: FraudEvidence) -> Result<WindowProof> {
        self.prove(evidence, WindowOutcome::Diverged)
    }

    /// The commitment's terminal window: an attestation that honest execution
    /// ends the trace, and with which output. Errors unless the guest ended it
    /// `TerminalMatch`. The mislabel proof (`chain defend`).
    pub fn prove_terminal(&self) -> Result<WindowProof> {
        let mut verifier = TraceVerifier::new(self.commitment.clone(), &EMPTY_LEAF, self.cfs)
            .map_err(|e| Error::Other(e.to_string()))?;
        let window = verifier
            .terminal_window(self.trace)
            .map_err(|e| Error::Other(e.to_string()))?;
        self.prove(window, WindowOutcome::TerminalMatch)
    }

    fn prove(&self, evidence: FraudEvidence, expected: WindowOutcome) -> Result<WindowProof> {
        let receipt = crate::commands::prove::prove(
            evidence,
            self.trace,
            self.cfs,
            self.recorder,
            self.replayer,
            self.input_manifest,
            self.commitment,
        );
        let journal: TransitionJournal = receipt
            .journal
            .decode()
            .map_err(|e| Error::Other(format!("failed to decode transition journal: {e}")))?;
        let outcome = WindowOutcome::of(&journal.current_state).ok_or_else(|| {
            Error::Other("transition journal records a window that has not ended".into())
        })?;
        if outcome != expected {
            return Err(Error::Other(format!(
                "the guest ended the window {outcome:?}, not {expected:?}"
            )));
        }
        // Every proof names the commitment this prover was built for.
        if journal.refuted_trace_commitment != self.commitment.header().digest() {
            return Err(Error::Other(
                "the proven window names a different trace commitment".into(),
            ));
        }
        Ok(WindowProof {
            receipt,
            journal,
            outcome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_endings_are_outcomes() {
        assert_eq!(
            WindowOutcome::of(&TransitionState::Finished),
            Some(WindowOutcome::Diverged)
        );
        assert_eq!(
            WindowOutcome::of(&TransitionState::TerminalMatch),
            Some(WindowOutcome::TerminalMatch)
        );
    }
}
