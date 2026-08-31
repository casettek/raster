use chain_stage_planner::*;
use raster::prelude::*;

/// Planner entrypoint.
///
/// `budget` is a committed external. The return value is this stage's whole
/// authorized output, and the chain reads it as the trip count of the repeat
/// block that follows.
#[sequence]
fn main(budget: u64) -> u64 {
    let budget = select!(u64, budget);
    let steps = call!(plan_steps, budget);
    raster::println!("planner → {:?} step(s)", steps);
    steps
}
