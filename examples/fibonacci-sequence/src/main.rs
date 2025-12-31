use raster::prelude::*;

#[tile]
fn fib_step(a: u64, b: u64) -> (u64, u64) {
    (b, a + b)
}

#[tile]
fn check_limit(value: u64, limit: u64) -> bool {
    value < limit
}

#[sequence]
fn fibonacci_sequence() {
    let mut a = 0u64;
    let mut b = 1u64;
    let limit = 1000u64;

    loop {
        if !check_limit(b, limit) {
            break;
        }
        println!("{}", b);
        (a, b) = fib_step(a, b);
    }
}

fn main() {
    println!("Fibonacci Sequence Example");
    // TODO: Execute the sequence
}
