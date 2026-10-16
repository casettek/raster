use chain_stage_report::*;
use raster::prelude::*;

/// Phase 3 entrypoint (terminal).
///
/// `stats` is bound by the chain to phase 2's authorized output. One tile
/// assembles the report; the sequence itself does no computation — it only
/// selects, calls, and rebinds.
#[sequence]
fn main(stats: Stats) -> Report {
    let label = select!(String, stats.clone().label);
    let count = select!(u64, stats.clone().count);
    let sum = select!(u64, stats.clone().sum);
    let max = select!(u64, stats.max);

    let mean = call!(mean_scaled, clone!(sum), clone!(count));

    let report = call!(build_report, label, count, sum, max, mean);
    raster::println!("phase3 report → {:?}", report);
    report
}
