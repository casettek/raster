//! RISC0 guest program for the FRAUD WINDOW aggregator
//! (`docs/proposals/transition-split.md`).
//!
//! Verifies every part segment's receipt against the part's image id compiled
//! into this guest, every tile replay claim, and the authorization journal;
//! then `fraud_window::check_window` stitches each part's segments, compares
//! the chains the parts share, and yields the `FraudWindowJournal` committed
//! here. It runs no check of the window's steps itself: its cost grows with the
//! number of segments and tile steps, not with what the steps do.

mod ids;

use risc0_zkvm::guest::env;
use risc0_zkvm::sha::Digest;
use risc0_zkvm::{MaybePruned, ReceiptClaim};

use guest_common::frame;
use raster_core::fraud_window::{check_window, FraudWindowParams, PartKind, PartSegmentJournal};

fn part_image_id(part: PartKind) -> [u8; 32] {
    match part {
        PartKind::Trace => ids::TRACE,
        PartKind::ControlFlow => ids::CONTROL_FLOW,
        PartKind::Storage => ids::STORAGE,
        PartKind::Transition => ids::TRANSITION,
    }
}

fn main() {
    // Host inputs, in write order: the number of segments, each segment's
    // journal bytes, then the params (window, program, replay claims,
    // authorization journal).
    let count: u32 = frame::read();
    let segments: Vec<PartSegmentJournal> = (0..count)
        .map(|_| {
            let bytes = frame::read_bytes();
            let journal = PartSegmentJournal::decode(&bytes)
                .expect("host must supply a part segment journal");
            // The journal names its part; the receipt must be that part's
            // guest's, whose id only this build knows.
            env::verify(Digest::from(part_image_id(journal.part)), &bytes)
                .expect("a part segment's receipt must verify against its part's guest");
            journal
        })
        .collect();
    let params: FraudWindowParams = frame::read();

    for claim in &params.replay_claims {
        let claim = ReceiptClaim::ok(
            Digest::from(claim.image_id),
            MaybePruned::Pruned(Digest::from(claim.journal_digest)),
        );
        env::verify_integrity(&claim).expect("a tile replay claim must verify");
    }
    let authorization = risc0_zkvm::serde::to_vec(&params.authorization_journal)
        .expect("an authorization journal always encodes");
    env::verify(Digest::from(ids::AUTHORIZATION), &authorization)
        .expect("the authorization journal must verify against the authorization guest");

    let journal = check_window(&params, &segments)
        .unwrap_or_else(|refusal| panic!("FRAUD WINDOW refused the window: {refusal}"));
    env::commit_slice(&journal.encode());
}
