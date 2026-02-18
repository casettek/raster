//! Utility types and traits for debugging and iteration.

/// Trait for displaying values in binary format.
pub trait DisplayBinary {
    /// Print the binary representation of this value.
    fn print_binary(&self);

    /// Get the binary representation as a string.
    fn to_binary_string(&self) -> String;
}

impl DisplayBinary for u8 {
    fn print_binary(&self) {
        print!("{}", self.to_binary_string());
    }

    fn to_binary_string(&self) -> String {
        let mut result = String::with_capacity(8);
        for bit in (0..8).rev() {
            if (self >> bit) & 1 == 1 {
                result.push('1');
            } else {
                result.push('0');
            }
        }
        result
    }
}

impl DisplayBinary for u64 {
    fn print_binary(&self) {
        for byte in self.to_le_bytes() {
            byte.print_binary();
            print!("  ");
        }
        println!();
    }

    fn to_binary_string(&self) -> String {
        self.to_le_bytes()
            .iter()
            .map(|b| b.to_binary_string())
            .collect::<Vec<_>>()
            .join("  ")
    }
}

impl DisplayBinary for Vec<u8> {
    fn print_binary(&self) {
        for byte in self {
            byte.print_binary();
            print!("  ");
        }
        println!();
    }

    fn to_binary_string(&self) -> String {
        self.iter()
            .map(|b| b.to_binary_string())
            .collect::<Vec<_>>()
            .join("  ")
    }
}

impl DisplayBinary for Vec<u64> {
    fn print_binary(&self) {
        for &x in self {
            x.print_binary();
        }
    }

    fn to_binary_string(&self) -> String {
        self.iter()
            .map(|x| x.to_binary_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

