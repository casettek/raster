use chain_stage_report::*;
use raster::prelude::*;

/// Phase 3 entrypoint (terminal).
///
/// `stats` is bound by the chain to phase 2's authorized output. The report is
/// assembled one line per tile call through a draft; the sequence itself does
/// no computation — it only selects, calls, and rebinds.
#[sequence]
fn main(stats: Stats) -> Report {
    let label = select!(String, stats.clone().label);
    let count = select!(u64, stats.clone().count);
    let sum = select!(u64, stats.clone().sum);
    let max = select!(u64, stats.max);

    let mean = call!(mean_scaled, clone!(sum), clone!(count));

    let draft = new!(Report);
    let draft = call!(set_report_title, label, draft);
    let draft = call!(push_metric, "count".to_string(), count, draft);
    let draft = call!(push_metric, "sum".to_string(), sum, draft);
    let draft = call!(push_metric, "max".to_string(), max, draft);
    let draft = call!(push_mean, mean, draft);

    let report = finalize(draft);
    raster::println!("phase3 report → {:?}", report);
    report
}
