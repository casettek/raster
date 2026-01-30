//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse_macro_input, visit::Visit, Expr, ExprCall, ExprPath, FnArg, ItemFn, Pat, ReturnType, Type,
};

/// Extract input types and parameter names from a function signature.
fn extract_inputs(input: &ItemFn) -> (Vec<&Type>, Vec<&syn::Ident>) {
    input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    return Some((&*pat_type.ty, &pat_ident.ident));
                }
            }
            None
        })
        .unzip()
}

/// Check if the function returns a Result type.
fn returns_result(input: &ItemFn) -> bool {
    matches!(&input.sig.output, ReturnType::Type(_, ty) if {
        let ty_str = ty.to_token_stream().to_string();
        ty_str.starts_with("Result") || ty_str.contains(":: Result")
    })
}

/// Generate only the input deserialization code.
///
/// Returns a TokenStream that deserializes input bytes into the appropriate variables.
fn gen_inputs_deserialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let (input_types, input_names) = extract_inputs(input);

    if input_types.is_empty() {
        // No arguments - no deserialization needed
        quote! {}
    } else if input_types.len() == 1 {
        // Single argument - deserialize directly
        let ty = input_types[0];
        let name = input_names[0];
        quote! {
            let #name: #ty = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let tuple_type = quote! { (#(#input_types),*) };
        quote! {
            let (#(#input_names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    }
}

/// Add output serialization to the code pipeline.
///
/// Wraps the provided code with serialization of `result` to `output`.
fn gen_output_serialization(_input: &ItemFn) -> proc_macro2::TokenStream {
    quote! {
        let output = ::raster::core::postcard::to_allocvec(&result)
            .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
    }
}

/// Generate input serialization code for tracing in the original function.
///
/// This serializes the typed input parameters to bytes for the trace emission.
fn gen_trace_input_serialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let (input_types, input_names) = extract_inputs(input);

    if input_types.is_empty() {
        quote! { let __raster_input_bytes: ::alloc::vec::Vec<u8> = ::alloc::vec::Vec::new(); }
    } else if input_types.len() == 1 {
        let name = input_names[0];
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&#name)
                .unwrap_or_default();
        }
    } else {
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&(#(&#input_names),*))
                .unwrap_or_default();
        }
    }
}

/// Generate only the function call code.
///
/// Returns a TokenStream that calls the function and stores the result.
fn gen_function_call(input: &ItemFn) -> proc_macro2::TokenStream {
    let fn_name = &input.sig.ident;
    let (_, input_names) = extract_inputs(input);
    let is_result = returns_result(input);

    if input_names.is_empty() {
        if is_result {
            quote! { let result = #fn_name()?; }
        } else {
            quote! { let result = #fn_name(); }
        }
    } else {
        if is_result {
            quote! { let result = #fn_name(#(#input_names),*)?; }
        } else {
            quote! { let result = #fn_name(#(#input_names),*); }
        }
    }
}

/// Parses tile attributes from the macro invocation.
///
/// Uses named argument `kind` for tile type: `#[tile(kind = iter)]` or `#[tile(kind = recur)]`.
struct TileAttrs {
    /// Tile type: "iter" (default) or "recur".
    tile_type: String,
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
    description: Option<String>,
}

impl TileAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = TileAttrs {
            tile_type: "iter".to_string(), // default to iter
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        if attr.is_empty() {
            return attrs; // Default to "iter" if no arguments
        }

        // Parse comma-separated key=value pairs
        let attr_str = attr.to_string();

        for part in attr_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                match key {
                    "kind" => match value {
                        "iter" | "recur" => attrs.tile_type = value.to_string(),
                        _ => panic!("Unknown tile kind '{}'. Valid kinds: iter, recur", value),
                    },
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

        attrs
    }
}

/// Marks a function as a Raster tile.
///
/// This macro:
/// 1. Injects tracing code into the original function (for std targets)
/// 2. Generates an ABI wrapper that handles bincode serialization/deserialization
/// 3. Registers the tile in the global `TILE_REGISTRY` distributed slice
///
/// # Attributes
/// - `kind = iter` - Standard iterative tile (default if not specified)
/// - `kind = recur` - Recursive tile for stateful computations
/// - `estimated_cycles = N` - Expected cycle count for resource estimation
/// - `max_memory = N` - Maximum memory usage in bytes
/// - `description = "..."` - Human-readable description
///
/// # Example
/// ```ignore
/// #[tile]
/// fn compute(input: u64) -> u64 {
///     input * 2
/// }
///
/// #[tile(kind = iter, estimated_cycles = 1000, description = "Greets a user")]
/// fn greet(name: String) -> String {
///     format!("Hello, {}!", name)
/// }
///
/// #[tile(kind = recur)]
/// fn iterate(state: State) -> State {
///     // recursive computation
/// }
/// ```
#[proc_macro_attribute]
pub fn tile(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name_str = input_fn.sig.ident.to_string();

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_sig = &input_fn.sig;
    let fn_attrs = &input_fn.attrs;
    let fn_body = &input_fn.block;

    let function_wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name);
    let registration_name = format_ident!(
        "__RASTER_TILE_REGISTRATION_{}",
        fn_name.to_string().to_uppercase()
    );

    let attrs = TileAttrs::parse(attr);

    // Generate optional metadata fields
    let estimated_cycles_expr = match attrs.estimated_cycles {
        Some(cycles) => quote! { ::core::option::Option::Some(#cycles) },
        None => quote! { ::core::option::Option::None },
    };

    let max_memory_expr = match attrs.max_memory {
        Some(memory) => quote! { ::core::option::Option::Some(#memory) },
        None => quote! { ::core::option::Option::None },
    };

    let description_expr = match &attrs.description {
        Some(desc) => {
            let desc_str = desc.as_str();
            quote! { ::core::option::Option::Some(#desc_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    // Generate deserialization and function call
    let inputs_deserialization = gen_inputs_deserialization(&input_fn);
    let function_call = gen_function_call(&input_fn);
    let output_serialization = gen_output_serialization(&input_fn);

    // For recursive tiles, also generate a macro with the same name that allows `tile_name!(args)` syntax
    let recursive_macro = if attrs.tile_type == "recur" {
        let macro_name = format_ident!("{}", fn_name);
        quote! {
            /// Macro wrapper for recursive tile invocation.
            /// Use `tile_name!(args)` to invoke this recursive tile.
            /// For native execution, this simply calls the underlying function.
            /// The `!` syntax signals to the CFS compiler that this tile should
            /// be executed recursively until its first output returns true.
            #[macro_export]
            macro_rules! #macro_name {
                ($($args:expr),* $(,)?) => {
                    #fn_name($($args),*)
                };
            }
        }
    } else {
        quote! {}
    };

    // Generate input parameter metadata for tracing: &[("name", "Type"), ...]
    let (input_types, input_names) = extract_inputs(&input_fn);
    let input_param_tuples: Vec<_> = input_names
        .iter()
        .zip(input_types.iter())
        .map(|(name, ty)| {
            let name_str = name.to_string();
            let ty_str = ty.to_token_stream().to_string();
            quote! { (#name_str, #ty_str) }
        })
        .collect();

    // Generate trace description expression (Option<&str>)
    let trace_desc_expr = match &attrs.description {
        Some(desc) => {
            let desc_str = desc.as_str();
            quote! { ::core::option::Option::Some(#desc_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    // Generate output type expression (Option<&str>)
    let trace_output_type_expr = match &input_fn.sig.output {
        ReturnType::Default => quote! { ::core::option::Option::None },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { ::core::option::Option::Some(#ty_str) }
        }
    };

    // Generate input serialization code for tracing
    let trace_input_serialization = gen_trace_input_serialization(&input_fn);

    let original_function = quote! {
        #(#fn_attrs)*
        #fn_vis #fn_sig {
            // On std + non-riscv32: wrap body in closure for tracing
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                // Serialize inputs for tracing
                #trace_input_serialization

                // Execute original body via closure to handle early returns
                let __raster_result = (|| #fn_body)();

                // Serialize output and emit trace
                let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_result)
                    .unwrap_or_default();

                ::raster::__emit_trace(
                    #fn_name_str,
                    #trace_desc_expr,
                    &[#(#input_param_tuples),*],
                    #trace_output_type_expr,
                    &__raster_input_bytes,
                    &__raster_output_bytes,
                );

                return __raster_result;
            }

            // On riscv32 or no-std: just execute original body directly
            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            #fn_body
        }
    };

    TokenStream::from(quote! {
        // Original function with tracing injected
        #original_function

        // Generate the ABI wrapper function (available on all platforms, no_std compatible)
        pub fn #function_wrapper_name(input: &[u8]) -> ::raster::core::Result<::alloc::vec::Vec<u8>> {
            #inputs_deserialization

            #function_call

            #output_serialization

            Ok(output)
        }

        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        #[::raster::core::linkme::distributed_slice(::raster::core::registry::TILE_REGISTRY)]
        #[linkme(crate = ::raster::core::linkme)]
        static #registration_name: ::raster::core::registry::TileRegistration =
        ::raster::core::registry::TileRegistration::new(
            ::raster::core::tile::TileMetadataStatic::new(
                #fn_name_str,
                #fn_name_str,
                #description_expr,
                #estimated_cycles_expr,
                #max_memory_expr
            ),
            #function_wrapper_name,
        );
    })
}

/// Parses optional sequence attributes from the macro invocation.
struct SequenceAttrs {
    description: Option<String>,
}

impl SequenceAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = SequenceAttrs { description: None };

        if attr.is_empty() {
            return attrs;
        }

        // Parse comma-separated key=value pairs
        let attr_str = attr.to_string();
        for part in attr_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                if key == "description" {
                    attrs.description = Some(value.to_string());
                }
            }
        }

        attrs
    }
}

/// Visitor that extracts function call names from the AST.
/// It collects function names in the order they appear.
struct TileCallExtractor {
    tile_calls: Vec<String>,
}

impl TileCallExtractor {
    fn new() -> Self {
        Self {
            tile_calls: Vec::new(),
        }
    }
}

impl<'ast> Visit<'ast> for TileCallExtractor {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        // Check if this is a simple function call (not a method call)
        if let Expr::Path(ExprPath { path, .. }) = &*node.func {
            // Get the function name (last segment of the path)
            if let Some(segment) = path.segments.last() {
                let fn_name = segment.ident.to_string();
                // Skip common non-tile functions
                if !is_excluded_function(&fn_name) {
                    self.tile_calls.push(fn_name.to_owned());
                }
            }
        }
        // Continue visiting nested expressions
        syn::visit::visit_expr_call(self, node);
    }
}

/// Check if a function name should be excluded from tile extraction.
/// These are common Rust functions that are not tiles.
fn is_excluded_function(name: &str) -> bool {
    matches!(
        name,
        // Common Rust functions that aren't tiles
        "println" | "print" | "eprintln" | "eprint" | "dbg" |
        "format" | "panic" | "assert" | "assert_eq" | "assert_ne" |
        "Some" | "None" | "Ok" | "Err" |
        "Box" | "Vec" | "String" | "to_string" | "to_owned" |
        "clone" | "into" | "from" | "default" |
        // Allocator functions
        "alloc" | "dealloc"
    )
}

/// Declares a sequence of tiles with linear control flow.
///
/// The `#[sequence]` macro parses the function body to extract tile calls
/// in the order they appear. The function remains callable for native execution,
/// and the sequence is registered for use with `cargo raster preview`.
///
/// # Attributes
/// - `description = "..."` - Human-readable description of the sequence
///
/// # Example
/// ```ignore
/// #[sequence]
/// fn main(name: String) -> String {
///     let greeting = greet(name);
///     exclaim(greeting)
/// }
/// ```
///
/// This will register a sequence named "main" with tiles `["greet", "exclaim"]`.
#[proc_macro_attribute]
pub fn sequence(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = SequenceAttrs::parse(attr);
    let input_fn = parse_macro_input!(item as ItemFn);

    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    let registration_name = format_ident!(
        "__RASTER_SEQUENCE_REGISTRATION_{}",
        fn_name.to_string().to_uppercase()
    );

    // Extract tile calls from the function body
    let mut extractor = TileCallExtractor::new();
    extractor.visit_item_fn(&input_fn);
    let tile_calls = extractor.tile_calls;

    // Generate the tile list as a static array
    let tile_count = tile_calls.len();
    let tile_strs: Vec<_> = tile_calls.iter().map(|s| s.as_str()).collect();

    // Generate description expression
    let description_expr = match &attrs.description {
        Some(desc) => {
            let desc_str = desc.as_str();
            quote! { ::core::option::Option::Some(#desc_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    let tiles_static_name = format_ident!(
        "__RASTER_SEQUENCE_TILES_{}",
        fn_name.to_string().to_uppercase()
    );

    let expanded = quote! {
        // Keep the original function unchanged for native execution
        #input_fn

        // Static array of tile IDs for this sequence
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        static #tiles_static_name: [&'static str; #tile_count] = [#(#tile_strs),*];

        // Register the sequence in the distributed slice (only on platforms that support linkme and std)
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        #[::raster::core::linkme::distributed_slice(::raster::core::registry::SEQUENCE_REGISTRY)]
        #[linkme(crate = ::raster::core::linkme)]
        static #registration_name: ::raster::core::registry::SequenceRegistration =
            ::raster::core::registry::SequenceRegistration::new(
                ::raster::core::registry::SequenceMetadataStatic::new(
                    #fn_name_str,
                    #fn_name_str,
                    #description_expr,
                ),
                &#tiles_static_name,
            );
    };

    TokenStream::from(expanded)
}

/// Entry point macro that handles raster CLI tile execution requests and input parsing.
///
/// Apply this to your main function to automatically handle subprocess
/// tile execution for the native backend, and optionally parse CLI input arguments.
///
/// # Basic Example (no input)
/// ```ignore
/// #[raster::main]
/// fn main() {
///     let result = greet_sequence("Raster".to_string());
///     println!("{}", result);
/// }
/// ```
///
/// # With Input Parameter
/// ```ignore
/// #[raster::main]
/// fn main(name: String) {
///     let result = greet_sequence(name);
///     println!("{}", result);
/// }
/// ```
///
/// Run with: `cargo run -- --input '"Raster"'`
///
/// The macro parses `--input <json>` from command line arguments and deserializes
/// it into the parameter type. Multiple parameters are deserialized from a JSON array.
///
/// # Expansion
///
/// `fn main(name: String)` expands to:
/// ```ignore
/// fn main() {
///     if ::raster::try_execute_tile_from_args() {
///         return;
///     }
///     
///     let name: String = ::raster::parse_main_input()
///         .expect("Failed to parse --input argument. Usage: --input '<json>'");
///     
///     let result = greet_sequence(name);
///     println!("{}", result);
/// }
/// ```
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    let fn_block = &input_fn.block;
    let fn_attrs = &input_fn.attrs;

    // Extract parameters from the function signature
    let params: Vec<_> = input_fn.sig.inputs.iter().collect();

    // Generate input parsing code based on parameters
    let input_parsing = if params.is_empty() {
        // No parameters - no input parsing needed
        quote! {}
    } else if params.len() == 1 {
        // Single parameter - deserialize directly
        let param = &params[0];
        if let FnArg::Typed(pat_type) = param {
            let pat = &pat_type.pat;
            let ty = &pat_type.ty;
            quote! {
                let #pat: #ty = ::raster::parse_main_input()
                    .expect("Failed to parse --input argument. Usage: --input '<json>'");
            }
        } else {
            quote! {}
        }
    } else {
        // Multiple parameters - deserialize as tuple
        let names: Vec<_> = params
            .iter()
            .filter_map(|p| {
                if let FnArg::Typed(pt) = p {
                    Some(&pt.pat)
                } else {
                    None
                }
            })
            .collect();
        let types: Vec<_> = params
            .iter()
            .filter_map(|p| {
                if let FnArg::Typed(pt) = p {
                    Some(&pt.ty)
                } else {
                    None
                }
            })
            .collect();
        quote! {
            let (#(#names),*): (#(#types),*) = ::raster::parse_main_input()
                .expect("Failed to parse --input argument. Usage: --input '[val1, val2, ...]'");
        }
    };

    let expanded = quote! {
        #(#fn_attrs)*
        fn main() {
            // Parse --commit and --verify flags from CLI args
            fn __parse_commit_verify() -> (Option<String>, Option<String>) {
                let args: Vec<String> = std::env::args().collect();
                let commit = args.iter().position(|a| a == "--commit")
                    .and_then(|i| args.get(i + 1).cloned());
                let verify = args.iter().position(|a| a == "--verify")
                    .and_then(|i| args.get(i + 1).cloned());
                (commit, verify)
            }

            let (commit_path, verify_path) = __parse_commit_verify();
            let bits = 16;

            // Initialize subscriber based on flags
            if let Some(path) = commit_path {
                let file = std::fs::File::create(&path)
                    .expect(&format!("Failed to create commit file: {}", path));
                let exec_commit_subscriber = ::raster::ExecCommitSubscriber::new(bits, file);
                ::raster::init_with(exec_commit_subscriber);
            } else if let Some(path) = verify_path {
                let exec_verify_subscriber = ::raster::ExecVerifySubscriber::new(
                    bits,
                    std::path::PathBuf::from(path),
                );
                ::raster::init_with(exec_verify_subscriber);
            } else {
                // Default: use JsonSubscriber for stdout output
                ::raster::init();
            }

            if ::raster::try_execute_tile_from_args() {
                return;
            }

            #input_parsing

            #fn_block

            ::raster::finish();
        }
    };

    TokenStream::from(expanded)
}
