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
    /// Number of input arguments.
    pub input_count: usize,
    /// Number of output values (1 for most functions, 0 for unit return).
    pub output_count: usize,
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
                            let (input_count, output_count) = self.extract_fn_arity(fn_line);
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
                                input_count,
                                output_count,
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

    /// Extract the input count and output count from a function signature.
    /// Returns (input_count, output_count).
    fn extract_fn_arity(&self, line: &str) -> (usize, usize) {
        let input_count = self.count_fn_args(line);
        let output_count = self.count_fn_outputs(line);
        (input_count, output_count)
    }

    /// Count the number of arguments in a function signature.
    fn count_fn_args(&self, line: &str) -> usize {
        // Find the argument list between ( and )
        let Some(start) = line.find('(') else {
            return 0;
        };
        
        // Find matching closing paren, handling nested parens
        let after_open = &line[start + 1..];
        let mut depth = 1;
        let mut end_pos = after_open.len();
        
        for (i, c) in after_open.chars().enumerate() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        
        let args_str = &after_open[..end_pos];
        
        // Empty args
        if args_str.trim().is_empty() {
            return 0;
        }
        
        // Count arguments by counting commas at depth 0, plus 1
        // Handle nested generics like Vec<(A, B)>
        let mut count = 1;
        let mut angle_depth: usize = 0;
        let mut paren_depth: usize = 0;
        
        for c in args_str.chars() {
            match c {
                '<' => angle_depth += 1,
                '>' => angle_depth = angle_depth.saturating_sub(1),
                '(' => paren_depth += 1,
                ')' => paren_depth = paren_depth.saturating_sub(1),
                ',' if angle_depth == 0 && paren_depth == 0 => count += 1,
                _ => {}
            }
        }
        
        count
    }

    /// Count the number of outputs from a function signature.
    /// Returns 0 for unit `()`, 1 for single values, or tuple element count for tuples.
    fn count_fn_outputs(&self, line: &str) -> usize {
        // Find the return type after ->
        let Some(arrow_pos) = line.find("->") else {
            // No return type means unit ()
            return 0;
        };
        
        let after_arrow = &line[arrow_pos + 2..];
        
        // Find the return type (ends at { or where or end of line)
        let end_pos = after_arrow
            .find(|c: char| c == '{' || c == '\n')
            .unwrap_or(after_arrow.len());
        
        let return_type = after_arrow[..end_pos].trim();
        
        // Check for unit type
        if return_type == "()" {
            return 0;
        }
        
        // Check for tuple - starts with ( and contains commas at depth 0
        if return_type.starts_with('(') {
            // Find matching )
            let mut depth = 0;
            let mut comma_count = 0;
            
            for c in return_type.chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 1 => comma_count += 1,
                    _ => {}
                }
            }
            
            // Tuple with N elements has N-1 commas
            if comma_count > 0 {
                return comma_count + 1;
            }
        }
        
        // Single value or non-tuple type
        1
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
    /// Ordered list of tile calls with their bindings.
    pub calls: Vec<SequenceCall>,
    /// Parameter names for the sequence function.
    pub param_names: Vec<String>,
    /// Number of input parameters.
    pub input_count: usize,
    /// The source file where the sequence was found.
    pub source_file: String,
    /// Line number where the sequence is defined.
    pub line_number: usize,
}

/// A function call within a sequence, capturing variable bindings.
#[derive(Debug, Clone)]
pub struct SequenceCall {
    /// The function being called (tile or sequence ID).
    pub callee: String,
    /// Variable name that receives the result (if any).
    pub result_binding: Option<String>,
    /// Arguments passed to the function (variable names or expressions).
    pub arguments: Vec<String>,
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
                            // Extract parameter names and count
                            let param_names = self.extract_param_names(fn_line);
                            let input_count = param_names.len();
                            
                            // Parse the function body to extract tile calls with bindings
                            let body_start = fn_line_idx;
                            let calls = self.extract_sequence_calls(&lines, body_start);

                            sequences.push(DiscoveredSequence {
                                id: fn_name.clone(),
                                name: fn_name,
                                description: attrs.description,
                                calls,
                                param_names,
                                input_count,
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

    /// Extract sequence calls from a function body with variable bindings.
    fn extract_sequence_calls(&self, lines: &[&str], start_line: usize) -> Vec<SequenceCall> {
        let mut calls = Vec::new();
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
                        return calls;
                    }
                }
            }

            // Skip the line where we first enter the body (function signature line)
            if entered_body_this_line && is_first_brace {
                is_first_brace = false;
                continue;
            }

            if in_body {
                let trimmed = line.trim();
                
                // Skip comments and macros
                if trimmed.starts_with("//") 
                    || trimmed.starts_with("println!")
                    || trimmed.starts_with("print!")
                    || trimmed.starts_with("format!")
                {
                    continue;
                }

                // For let statements: let binding = call(args)
                if trimmed.starts_with("let ") {
                    if let Some(call) = self.parse_let_statement(trimmed) {
                        if !self.is_excluded(&call.callee) {
                            calls.push(call);
                        }
                    }
                    continue;
                }

                // For expression statements (return expressions, bare calls)
                if let Some(call) = self.parse_expression_call(trimmed) {
                    if !self.is_excluded(&call.callee) {
                        calls.push(call);
                    }
                }
            }
        }

        calls
    }

    /// Parse a let statement like "let greeting = greet(name);"
    fn parse_let_statement(&self, line: &str) -> Option<SequenceCall> {
        // Remove "let " prefix
        let after_let = line.strip_prefix("let ")?.trim();
        
        // Find the = sign
        let eq_pos = after_let.find('=')?;
        let binding = after_let[..eq_pos].trim().to_string();
        let rhs = after_let[eq_pos + 1..].trim();
        
        // Parse the function call from the RHS
        self.parse_call_expr(rhs, Some(binding))
    }

    /// Parse a bare expression call like "exclaim(greeting)" or return expression
    fn parse_expression_call(&self, line: &str) -> Option<SequenceCall> {
        self.parse_call_expr(line, None)
    }

    /// Parse a function call expression and extract callee and arguments.
    fn parse_call_expr(&self, expr: &str, result_binding: Option<String>) -> Option<SequenceCall> {
        let trimmed = expr.trim().trim_end_matches(';');
        
        // Find the function name (identifier before first '(')
        let paren_pos = trimmed.find('(')?;
        let callee = trimmed[..paren_pos].trim();
        
        // Skip if callee contains path separators or is a method call
        if callee.contains("::") || callee.contains('.') || callee.is_empty() {
            return None;
        }
        
        // Extract arguments from between ( and )
        let args_start = paren_pos + 1;
        let mut depth = 1;
        let mut args_end = trimmed.len();
        
        for (i, c) in trimmed[args_start..].chars().enumerate() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        args_end = args_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        
        let args_str = &trimmed[args_start..args_end];
        let arguments = self.parse_arguments(args_str);
        
        Some(SequenceCall {
            callee: callee.to_string(),
            result_binding,
            arguments,
        })
    }

    /// Parse comma-separated arguments, handling nested parens and generics.
    fn parse_arguments(&self, args_str: &str) -> Vec<String> {
        if args_str.trim().is_empty() {
            return Vec::new();
        }
        
        let mut args = Vec::new();
        let mut current_arg = String::new();
        let mut paren_depth: usize = 0;
        let mut angle_depth: usize = 0;
        
        for c in args_str.chars() {
            match c {
                '(' => {
                    paren_depth += 1;
                    current_arg.push(c);
                }
                ')' => {
                    paren_depth = paren_depth.saturating_sub(1);
                    current_arg.push(c);
                }
                '<' => {
                    angle_depth += 1;
                    current_arg.push(c);
                }
                '>' => {
                    angle_depth = angle_depth.saturating_sub(1);
                    current_arg.push(c);
                }
                ',' if paren_depth == 0 && angle_depth == 0 => {
                    args.push(current_arg.trim().to_string());
                    current_arg = String::new();
                }
                _ => current_arg.push(c),
            }
        }
        
        // Don't forget the last argument
        let last = current_arg.trim();
        if !last.is_empty() {
            args.push(last.to_string());
        }
        
        args
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

    /// Extract parameter names from a function signature.
    fn extract_param_names(&self, line: &str) -> Vec<String> {
        // Find args between ( and )
        let Some(start) = line.find('(') else {
            return Vec::new();
        };
        
        let after_open = &line[start + 1..];
        let mut depth = 1;
        let mut end_pos = after_open.len();
        
        for (i, c) in after_open.chars().enumerate() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        
        let args_str = &after_open[..end_pos];
        if args_str.trim().is_empty() {
            return Vec::new();
        }
        
        // Parse each parameter, extracting just the name (before the colon)
        let mut names = Vec::new();
        let mut current = String::new();
        let mut angle_depth: usize = 0;
        
        for c in args_str.chars() {
            match c {
                '<' => {
                    angle_depth += 1;
                    current.push(c);
                }
                '>' => {
                    angle_depth = angle_depth.saturating_sub(1);
                    current.push(c);
                }
                ',' if angle_depth == 0 => {
                    if let Some(name) = self.extract_param_name(&current) {
                        names.push(name);
                    }
                    current = String::new();
                }
                _ => current.push(c),
            }
        }
        
        // Last parameter
        if let Some(name) = self.extract_param_name(&current) {
            names.push(name);
        }
        
        names
    }

    /// Extract parameter name from "name: Type" pattern.
    fn extract_param_name(&self, param: &str) -> Option<String> {
        let param = param.trim();
        if param.is_empty() {
            return None;
        }
        
        // Find the colon separating name from type
        let colon_pos = param.find(':')?;
        let name = param[..colon_pos].trim();
        
        // Handle patterns like "mut name"
        let name = name.strip_prefix("mut ").unwrap_or(name).trim();
        
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
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

    #[test]
    fn test_count_fn_args() {
        let discovery = TileDiscovery::new(".");
        
        assert_eq!(discovery.count_fn_args("fn foo() -> u64 {"), 0);
        assert_eq!(discovery.count_fn_args("fn greet(name: String) -> String {"), 1);
        assert_eq!(discovery.count_fn_args("fn add(a: u64, b: u64) -> u64 {"), 2);
        assert_eq!(discovery.count_fn_args("fn complex(a: Vec<(u64, u64)>, b: String) -> u64 {"), 2);
    }

    #[test]
    fn test_count_fn_outputs() {
        let discovery = TileDiscovery::new(".");
        
        assert_eq!(discovery.count_fn_outputs("fn foo() {"), 0);
        assert_eq!(discovery.count_fn_outputs("fn foo() -> () {"), 0);
        assert_eq!(discovery.count_fn_outputs("fn foo() -> u64 {"), 1);
        assert_eq!(discovery.count_fn_outputs("fn foo() -> String {"), 1);
        assert_eq!(discovery.count_fn_outputs("fn foo() -> (u64, String) {"), 2);
        assert_eq!(discovery.count_fn_outputs("fn foo() -> (u64, String, bool) {"), 3);
    }

    #[test]
    fn test_extract_param_names() {
        let discovery = SequenceDiscovery::new(".");
        
        assert_eq!(
            discovery.extract_param_names("fn foo() {"),
            Vec::<String>::new()
        );
        assert_eq!(
            discovery.extract_param_names("fn greet(name: String) -> String {"),
            vec!["name".to_string()]
        );
        assert_eq!(
            discovery.extract_param_names("fn add(a: u64, b: u64) -> u64 {"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn test_parse_let_statement() {
        let discovery = SequenceDiscovery::new(".");
        
        let call = discovery.parse_let_statement("let greeting = greet(name);").unwrap();
        assert_eq!(call.callee, "greet");
        assert_eq!(call.result_binding, Some("greeting".to_string()));
        assert_eq!(call.arguments, vec!["name".to_string()]);
        
        let call = discovery.parse_let_statement("let result = add(a, b);").unwrap();
        assert_eq!(call.callee, "add");
        assert_eq!(call.arguments, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_parse_expression_call() {
        let discovery = SequenceDiscovery::new(".");
        
        let call = discovery.parse_expression_call("exclaim(greeting)").unwrap();
        assert_eq!(call.callee, "exclaim");
        assert_eq!(call.result_binding, None);
        assert_eq!(call.arguments, vec!["greeting".to_string()]);
    }
}

