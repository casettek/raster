//! Source-based tile discovery.
//!
//! This module scans Rust source files to find `#[tile]` annotations
//! and extracts tile metadata. This is necessary because the CLI binary
//! cannot access the user's tile registry (linkme distributed slices
//! are per-binary).

use raster_core::tile::{TileId, TileMetadata};
use raster_core::{Error, Result};
use std::fs;
use std::path::Path;

/// A discovered tile from source code analysis.
#[derive(Debug, Clone)]
pub struct DiscoveredTile {
    /// The tile metadata extracted from source.
    pub metadata: TileMetadata,
    /// The source file where the tile was found.
    pub source_file: String,
    /// Line number where the tile is defined.
    pub line_number: usize,
}

/// Discovers tiles by scanning source files.
pub struct TileDiscovery {
    /// Root directory to scan.
    root: std::path::PathBuf,
}

impl TileDiscovery {
    /// Create a new tile discovery instance for the given project root.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Discover all tiles in the project.
    pub fn discover(&self) -> Result<Vec<DiscoveredTile>> {
        let src_dir = self.root.join("src");
        if !src_dir.exists() {
            return Err(Error::Other(format!(
                "Source directory not found: {}",
                src_dir.display()
            )));
        }

        let mut tiles = Vec::new();
        self.scan_directory(&src_dir, &mut tiles)?;
        Ok(tiles)
    }

    /// Recursively scan a directory for Rust source files.
    fn scan_directory(&self, dir: &Path, tiles: &mut Vec<DiscoveredTile>) -> Result<()> {
        let entries = fs::read_dir(dir).map_err(Error::Io)?;

        for entry in entries {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();

            if path.is_dir() {
                self.scan_directory(&path, tiles)?;
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                self.scan_file(&path, tiles)?;
            }
        }

        Ok(())
    }

    /// Scan a single Rust source file for tile definitions.
    fn scan_file(&self, path: &Path, tiles: &mut Vec<DiscoveredTile>) -> Result<()> {
        let content = fs::read_to_string(path).map_err(Error::Io)?;
        let source_file = path.to_string_lossy().to_string();

        // Parse looking for #[tile] or #[tile(...)] followed by fn
        let lines: Vec<&str> = content.lines().collect();

        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();

            // Check if this line contains a #[tile] attribute
            if line.starts_with("#[tile") {
                // Extract the tile attributes if any
                let attrs = self.parse_tile_attrs(line);

                // Look for the function definition on subsequent lines
                let mut fn_line_idx = i + 1;
                while fn_line_idx < lines.len() {
                    let fn_line = lines[fn_line_idx].trim();
                    
                    // Skip empty lines, comments, and other attributes
                    if fn_line.is_empty() 
                        || fn_line.starts_with("//") 
                        || fn_line.starts_with("#[") 
                    {
                        fn_line_idx += 1;
                        continue;
                    }

                    // Check for function definition
                    if fn_line.starts_with("fn ") || fn_line.starts_with("pub fn ") {
                        if let Some(fn_name) = self.extract_fn_name(fn_line) {
                            tiles.push(DiscoveredTile {
                                metadata: TileMetadata {
                                    id: TileId(fn_name.clone()),
                                    name: fn_name,
                                    description: attrs.description,
                                    estimated_cycles: attrs.estimated_cycles,
                                    max_memory: attrs.max_memory,
                                },
                                source_file: source_file.clone(),
                                line_number: fn_line_idx + 1, // 1-indexed
                            });
                        }
                    }
                    break;
                }
            }

            i += 1;
        }

        Ok(())
    }

    /// Parse tile attributes from the macro invocation.
    fn parse_tile_attrs(&self, line: &str) -> TileAttrs {
        let mut attrs = TileAttrs::default();

        // Extract content between parentheses if present
        if let Some(start) = line.find('(') {
            if let Some(end) = line.rfind(')') {
                let content = &line[start + 1..end];
                
                for part in content.split(',') {
                    let part = part.trim();
                    if let Some((key, value)) = part.split_once('=') {
                        let key = key.trim();
                        let value = value.trim().trim_matches('"');
                        
                        match key {
                            "estimated_cycles" => {
                                attrs.estimated_cycles = value.parse().ok();
                            }
                            "max_memory" => {
                                attrs.max_memory = value.parse().ok();
                            }
                            "description" => {
                                attrs.description = Some(value.to_string());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        attrs
    }

    /// Extract the function name from a function definition line.
    fn extract_fn_name(&self, line: &str) -> Option<String> {
        // Handle both "fn name" and "pub fn name"
        let after_fn = if line.starts_with("pub fn ") {
            &line[7..]
        } else if line.starts_with("fn ") {
            &line[3..]
        } else {
            return None;
        };

        // Function name ends at ( or <
        let end = after_fn
            .find(|c: char| c == '(' || c == '<' || c.is_whitespace())
            .unwrap_or(after_fn.len());

        let name = after_fn[..end].trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }
}

#[derive(Default)]
struct TileAttrs {
    description: Option<String>,
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
}

// ============================================================================
// Sequence Discovery
// ============================================================================

/// A discovered sequence from source code analysis.
#[derive(Debug, Clone)]
pub struct DiscoveredSequence {
    /// The sequence ID (function name).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Ordered list of tile IDs in this sequence.
    pub tiles: Vec<String>,
    /// The source file where the sequence was found.
    pub source_file: String,
    /// Line number where the sequence is defined.
    pub line_number: usize,
}

/// Discovers sequences by scanning source files.
pub struct SequenceDiscovery {
    /// Root directory to scan.
    root: std::path::PathBuf,
}

impl SequenceDiscovery {
    /// Create a new sequence discovery instance for the given project root.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Discover all sequences in the project.
    pub fn discover(&self) -> Result<Vec<DiscoveredSequence>> {
        let src_dir = self.root.join("src");
        if !src_dir.exists() {
            return Err(Error::Other(format!(
                "Source directory not found: {}",
                src_dir.display()
            )));
        }

        let mut sequences = Vec::new();
        self.scan_directory(&src_dir, &mut sequences)?;
        Ok(sequences)
    }

    /// Recursively scan a directory for Rust source files.
    fn scan_directory(&self, dir: &Path, sequences: &mut Vec<DiscoveredSequence>) -> Result<()> {
        let entries = fs::read_dir(dir).map_err(Error::Io)?;

        for entry in entries {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();

            if path.is_dir() {
                self.scan_directory(&path, sequences)?;
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                self.scan_file(&path, sequences)?;
            }
        }

        Ok(())
    }

    /// Scan a single Rust source file for sequence definitions.
    fn scan_file(&self, path: &Path, sequences: &mut Vec<DiscoveredSequence>) -> Result<()> {
        let content = fs::read_to_string(path).map_err(Error::Io)?;
        let source_file = path.to_string_lossy().to_string();

        let lines: Vec<&str> = content.lines().collect();

        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();

            // Check if this line contains a #[sequence] attribute
            if line.starts_with("#[sequence") {
                // Extract sequence attributes
                let attrs = self.parse_sequence_attrs(line);

                // Look for the function definition on subsequent lines
                let mut fn_line_idx = i + 1;
                while fn_line_idx < lines.len() {
                    let fn_line = lines[fn_line_idx].trim();

                    // Skip empty lines, comments, and other attributes
                    if fn_line.is_empty()
                        || fn_line.starts_with("//")
                        || fn_line.starts_with("#[")
                    {
                        fn_line_idx += 1;
                        continue;
                    }

                    // Check for function definition
                    if fn_line.starts_with("fn ") || fn_line.starts_with("pub fn ") {
                        if let Some(fn_name) = self.extract_fn_name(fn_line) {
                            // Now we need to parse the function body to extract tile calls
                            let body_start = fn_line_idx;
                            let tiles = self.extract_tile_calls(&lines, body_start);

                            sequences.push(DiscoveredSequence {
                                id: fn_name.clone(),
                                name: fn_name,
                                description: attrs.description,
                                tiles,
                                source_file: source_file.clone(),
                                line_number: fn_line_idx + 1, // 1-indexed
                            });
                        }
                    }
                    break;
                }
            }

            i += 1;
        }

        Ok(())
    }

    /// Parse sequence attributes from the macro invocation.
    fn parse_sequence_attrs(&self, line: &str) -> SequenceAttrs {
        let mut attrs = SequenceAttrs::default();

        // Extract content between parentheses if present
        if let Some(start) = line.find('(') {
            if let Some(end) = line.rfind(')') {
                let content = &line[start + 1..end];

                for part in content.split(',') {
                    let part = part.trim();
                    if let Some((key, value)) = part.split_once('=') {
                        let key = key.trim();
                        let value = value.trim().trim_matches('"');

                        if key == "description" {
                            attrs.description = Some(value.to_string());
                        }
                    }
                }
            }
        }

        attrs
    }

    /// Extract function calls from a function body.
    /// This is a simple parser that looks for function call patterns.
    fn extract_tile_calls(&self, lines: &[&str], start_line: usize) -> Vec<String> {
        let mut tiles = Vec::new();
        let mut brace_depth = 0;
        let mut in_body = false;
        let mut is_first_brace = true;

        for line in lines.iter().skip(start_line) {
            // Count braces to track function body
            let mut entered_body_this_line = false;
            for c in line.chars() {
                if c == '{' {
                    brace_depth += 1;
                    if !in_body {
                        in_body = true;
                        entered_body_this_line = true;
                    }
                } else if c == '}' {
                    brace_depth -= 1;
                    if brace_depth == 0 && in_body {
                        return tiles;
                    }
                }
            }

            // Skip the line where we first enter the body (function signature line)
            if entered_body_this_line && is_first_brace {
                is_first_brace = false;
                continue;
            }

            if in_body {
                // Look for function call patterns: identifier followed by (
                // This is a simple regex-like pattern match
                let trimmed = line.trim();
                
                // Skip common non-tile patterns
                if trimmed.starts_with("//") 
                    || trimmed.starts_with("println!")
                    || trimmed.starts_with("print!")
                    || trimmed.starts_with("format!")
                {
                    continue;
                }

                // For let statements, extract the function call on the right side
                if trimmed.starts_with("let ") {
                    if let Some(eq_pos) = trimmed.find('=') {
                        let rhs = &trimmed[eq_pos + 1..];
                        if let Some(call) = self.extract_call_from_expr(rhs) {
                            if !self.is_excluded(&call) {
                                tiles.push(call);
                            }
                        }
                    }
                    continue;
                }

                // Try to extract a function call from expression statements
                if let Some(call) = self.extract_call_from_expr(trimmed) {
                    if !self.is_excluded(&call) {
                        tiles.push(call);
                    }
                }
            }
        }

        tiles
    }

    /// Extract a function call name from an expression.
    fn extract_call_from_expr(&self, expr: &str) -> Option<String> {
        let trimmed = expr.trim();
        
        // Look for pattern: identifier(
        // Find the first identifier followed by (
        let mut chars = trimmed.chars().peekable();
        let mut name = String::new();
        
        while let Some(c) = chars.next() {
            if c.is_alphanumeric() || c == '_' {
                name.push(c);
            } else if c == '(' && !name.is_empty() {
                return Some(name);
            } else if c == '.' || c == ':' {
                // Method call or path - reset
                name.clear();
            } else if c.is_whitespace() {
                // Keep going
                continue;
            } else {
                // Other character - reset if not alphanumeric
                if !name.is_empty() && c == '(' {
                    return Some(name);
                }
                name.clear();
            }
        }
        
        None
    }

    /// Check if a function name should be excluded from tile extraction.
    fn is_excluded(&self, name: &str) -> bool {
        matches!(
            name,
            "println" | "print" | "eprintln" | "eprint" | "dbg" |
            "format" | "panic" | "assert" | "assert_eq" | "assert_ne" |
            "Some" | "None" | "Ok" | "Err" |
            "Box" | "Vec" | "String" | "to_string" | "to_owned" |
            "clone" | "into" | "from" | "default"
        )
    }

    /// Extract the function name from a function definition line.
    fn extract_fn_name(&self, line: &str) -> Option<String> {
        // Handle both "fn name" and "pub fn name"
        let after_fn = if line.starts_with("pub fn ") {
            &line[7..]
        } else if line.starts_with("fn ") {
            &line[3..]
        } else {
            return None;
        };

        // Function name ends at ( or <
        let end = after_fn
            .find(|c: char| c == '(' || c == '<' || c.is_whitespace())
            .unwrap_or(after_fn.len());

        let name = after_fn[..end].trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }
}

#[derive(Default)]
struct SequenceAttrs {
    description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_fn_name() {
        let discovery = TileDiscovery::new(".");
        
        assert_eq!(
            discovery.extract_fn_name("fn greet(name: String) -> String {"),
            Some("greet".to_string())
        );
        assert_eq!(
            discovery.extract_fn_name("pub fn compute(x: u64) -> u64 {"),
            Some("compute".to_string())
        );
        assert_eq!(
            discovery.extract_fn_name("fn generic<T>(x: T) -> T {"),
            Some("generic".to_string())
        );
    }

    #[test]
    fn test_parse_tile_attrs() {
        let discovery = TileDiscovery::new(".");
        
        let attrs = discovery.parse_tile_attrs("#[tile]");
        assert!(attrs.description.is_none());
        assert!(attrs.estimated_cycles.is_none());
        
        let attrs = discovery.parse_tile_attrs("#[tile(description = \"Test tile\")]");
        assert_eq!(attrs.description, Some("Test tile".to_string()));
        
        let attrs = discovery.parse_tile_attrs(
            "#[tile(estimated_cycles = 1000, description = \"Complex\")]"
        );
        assert_eq!(attrs.estimated_cycles, Some(1000));
        assert_eq!(attrs.description, Some("Complex".to_string()));
    }
}

