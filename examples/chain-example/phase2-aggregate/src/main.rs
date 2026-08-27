use chain_stage_aggregate::*;
use raster::prelude::*;

/// Phase 2 entrypoint.
///
/// `filtered` is bound by the chain to phase 1's authorized output. We fold its
/// kept samples into `Stats`, our authorized output, which the chain feeds to
/// phase 3 as its `stats` parameter.
#[sequence]
fn main(filtered: Filtered) -> Stats {
    let label = select!(String, filtered.clone().label);
    let kept = select!(List<u64>, filtered.kept);

    let acc = call_recur!(
        tile = fold_stats,
        input = kept,
        state = Acc {
            count: 0,
            sum: 0,
            max: 0
        },
        args = ()
    );

    let count = select!(u64, acc.clone().count);
    let sum = select!(u64, acc.clone().sum);
    let max = select!(u64, acc.max);

    let stats = call!(assemble_stats, label, count, sum, max);
    raster::println!("phase2 aggregate → {:?}", stats);
    stats
}
