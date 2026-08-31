use chain_stage_step::*;
use raster::prelude::*;

/// Step entrypoint.
///
/// `prev` is bound by the chain to the previous iteration's output — or, at the
/// block's first iteration, to whatever the binding's `first` names. The stage
/// itself cannot tell the difference, which is the point: the manifest carries
/// the topology.
#[sequence]
fn main(prev: u64) -> u64 {
    let prev = select!(u64, prev);
    let next = call!(advance, prev);
    raster::println!("step → {:?}", next);
    next
}
