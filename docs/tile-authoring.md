# Tile Authoring Guide

## What is a Tile?

A tile is an isolated compute unit that:

- Takes explicit inputs
- Produces explicit outputs
- Can be compiled and executed independently
- Generates trace data when run

## Basic Tile

```rust
use raster::prelude::*;

#[tile]
fn double(x: u64) -> u64 {
    x * 2
}
```

The `#[tile]` macro:

- Registers the function as a tile
- Generates metadata (ID, signature, resource hints)
- Wraps execution with tracing hooks

## Tile Guidelines

### Keep Tiles Focused

Good:
```rust
#[tile]
fn validate_signature(pubkey: &[u8], signature: &[u8], message: &[u8]) -> bool {
    // Single responsibility: signature validation
}
```

Bad:
```rust
#[tile]
fn process_transaction(tx: Transaction) -> Result<Receipt> {
    // Too broad: validation, execution, storage
}
```

### Use Explicit Types

Good:
```rust
#[tile]
fn hash_data(data: Vec<u8>) -> [u8; 32] {
    // Clear input/output types
}
```

Bad:
```rust
#[tile]
fn hash_data(data: &dyn AsRef<[u8]>) -> Vec<u8> {
    // Unclear contract
}
```

### Avoid Hidden State

Good:
```rust
#[tile]
fn increment(value: u64, delta: u64) -> u64 {
    value + delta
}
```

Bad:
```rust
static mut COUNTER: u64 = 0;

#[tile]
fn increment() -> u64 {
    unsafe { COUNTER += 1; COUNTER }
}
```

## Sequences

Sequences describe how tiles are composed:

```rust
#[sequence]
fn verify_and_execute() {
    let valid = validate_signature(&pubkey, &sig, &msg);
    if valid {
        execute_transaction(&msg);
    }
}
```

The `#[sequence]` macro:

- Parses control flow
- Generates a schema (JSON/TOML)
- Does NOT generate executable code

## Resource Hints

Provide estimates to help cost analysis:

```rust
#[tile(estimated_cycles = 1000, max_memory = 4096)]
fn expensive_operation(input: Vec<u8>) -> Vec<u8> {
    // Computationally intensive work
}
```

## Testing

Test tiles as normal Rust functions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_double() {
        assert_eq!(double(5), 10);
    }
}
```

For integration tests, use the runtime:

```rust
#[test]
fn test_sequence() {
    let schema = SequenceSchema::load("my_sequence.json")?;
    let mut executor = Executor::new(FileTracer::new(...));
    let result = executor.execute(&schema)?;
    assert!(result.trace.is_some());
}
```

## Best Practices

1. **Tile Granularity**: Balance between too fine (overhead) and too coarse (inflexibility)
2. **Determinism**: Tiles should be pure functions when possible
3. **Serialization**: Use standard formats (JSON, bincode) for inputs/outputs
4. **Error Handling**: Return `Result` types for fallible operations
5. **Documentation**: Document expected behavior and resource usage

## Common Patterns

### Map-Reduce

```rust
#[tile]
fn map(items: Vec<u64>) -> Vec<u64> {
    items.iter().map(|x| x * 2).collect()
}

#[tile]
fn reduce(items: Vec<u64>) -> u64 {
    items.iter().sum()
}

#[sequence]
fn map_reduce() {
    let mapped = map(vec![1, 2, 3, 4]);
    let sum = reduce(mapped);
}
```

### Pipeline

```rust
#[tile]
fn parse(input: String) -> Data { ... }

#[tile]
fn validate(data: Data) -> ValidatedData { ... }

#[tile]
fn execute(data: ValidatedData) -> Result { ... }

#[sequence]
fn pipeline() {
    let data = parse(input);
    let validated = validate(data);
    execute(validated);
}
```

### Conditional Branching

```rust
#[tile]
fn check_condition(value: u64) -> bool { ... }

#[tile]
fn path_a(value: u64) -> u64 { ... }

#[tile]
fn path_b(value: u64) -> u64 { ... }

#[sequence]
fn conditional() {
    if check_condition(value) {
        path_a(value);
    } else {
        path_b(value);
    }
}
```
