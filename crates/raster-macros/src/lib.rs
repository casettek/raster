//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{parse_macro_input, visit::Visit, Expr, ExprCall, ExprPath, FnArg, ItemFn, Pat, ReturnType, Type};

/// Convert a syn::Type to a string representation for trace metadata.
fn type_to_string(ty: &Type) -> String {
    ty.to_token_stream().to_string().replace(" ", "")
}

/// Generate the deserialization and call logic for a tile function.
///
/// This handles three cases:
/// 1. No arguments - just call the function
/// 2. Single argument - deserialize directly from input bytes
/// 3. Multiple arguments - deserialize as a tuple from input bytes
fn add_serialization(
    fn_name: &syn::Ident,
    input_types: &[&Type],
    input_names: &[&syn::Ident],
    returns_result: bool,
) -> proc_macro2::TokenStream {
    if input_types.is_empty() {
        // No arguments
        if returns_result {
            quote! {
                let result = #fn_name()?;
                let output = ::raster::core::postcard::to_allocvec(&result)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
            }
        } else {
            quote! {
                let result = #fn_name();
                let output = ::raster::core::postcard::to_allocvec(&result)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
            }
        }
    } else if input_types.len() == 1 {
        // Single argument - deserialize directly
        let ty = input_types[0];
        let name = input_names[0];
        if returns_result {
            quote! {
                let #name: #ty = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#name)?;
                let output = ::raster::core::postcard::to_allocvec(&result)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
            }
        } else {
            quote! {
                let #name: #ty = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#name);
                let output = ::raster::core::postcard::to_allocvec(&result)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
            }
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let tuple_type = quote! { (#(#input_types),*) };
        if returns_result {
            quote! {
                let (#(#input_names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#(#input_names),*)?;
                let output = ::raster::core::postcard::to_allocvec(&result)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
            }
        } else {
            quote! {
                let (#(#input_names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#(#input_names),*);
                let output = ::raster::core::postcard::to_allocvec(&result)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))?;
            }
        }
    }
}

/// Parses optional tile attributes from the macro invocation.
struct TileAttrs {
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
    description: Option<String>,
}

impl TileAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = TileAttrs {
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

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

        attrs
    }
}

/// Marks a function as a Raster tile.
///
/// This macro:
/// 1. Preserves the original function unchanged
/// 2. Generates an ABI wrapper that handles bincode serialization/deserialization
/// 3. Registers the tile in the global `TILE_REGISTRY` distributed slice
///
/// # Attributes
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
/// #[tile(estimated_cycles = 1000, description = "Greets a user")]
/// fn greet(name: String) -> String {
///     format!("Hello, {}!", name)
/// }
/// ```
#[proc_macro_attribute]
pub fn tile(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = TileAttrs::parse(attr);
    let input_fn = parse_macro_input!(item as ItemFn);

    let with_tracing = add_tracing(&input_fn);

    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();

    let wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name);

    // Extract input types for deserialization
    let input_types: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => Some(&*pat_type.ty),
            FnArg::Receiver(_) => None,
        })
        .collect();

    // Extract input pattern names for calling the original function
    let input_names: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(pat_ident) => Some(&pat_ident.ident),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect();

    // Extract input parameter names as string literals for trace metadata
    let input_name_strs: Vec<String> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(pat_ident) => Some(pat_ident.ident.to_string()),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect();

    // Extract input type names as string literals for trace metadata
    let input_type_strs: Vec<String> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => Some(type_to_string(&pat_type.ty)),
            FnArg::Receiver(_) => None,
        })
        .collect();

    // Extract return type as a string for trace metadata
    let output_type_str: Option<String> = match &input_fn.sig.output {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => Some(type_to_string(ty)),
    };

    // Check if function returns a Result or a plain value
    let returns_result = match &input_fn.sig.output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => {
            if let Type::Path(type_path) = &**ty {
                type_path
                    .path
                    .segments
                    .last()
                    .map(|seg| seg.ident == "Result")
                    .unwrap_or(false)
            } else {
                false
            }
        }
    };


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

    // Generate description expression for emit_trace (needs to be used in multiple places)
    let description_for_trace = match &attrs.description {
        Some(desc) => {
            let desc_str = desc.as_str();
            quote! { ::core::option::Option::Some(#desc_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    // Generate input params array for emit_trace: &[("name", "type"), ...]
    let input_params_tokens = if input_name_strs.is_empty() {
        quote! { &[] }
    } else {
        let pairs: Vec<_> = input_name_strs
            .iter()
            .zip(input_type_strs.iter())
            .map(|(name, ty)| {
                quote! { (#name, #ty) }
            })
            .collect();
        quote! { &[#(#pairs),*] }
    };

    // Generate output type expression for emit_trace
    let output_type_for_trace = match &output_type_str {
        Some(ty_str) => {
            quote! { ::core::option::Option::Some(#ty_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    // Generate the deserialization and call logic based on number of args
    let fn_call_with_serialization = add_serialization(
        fn_name,
        &input_types,
        &input_names,
        returns_result,
    );


    let expanded = quote! {
        // Keep the original function unchanged
        #input_fn

        // Generate the ABI wrapper function (available on all platforms, no_std compatible)
        pub fn #wrapper_name(input: &[u8]) -> ::raster::core::Result<::alloc::vec::Vec<u8>> {
            #fn_call_with_serialization

            Ok(output)
        }
    };

    TokenStream::from(expanded)
}

/// Parses optional sequence attributes from the macro invocation.
struct SequenceAttrs {
    description: Option<String>,
}

impl SequenceAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = SequenceAttrs {
            description: None,
        };

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
        Self { tile_calls: Vec::new() }
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
    let registration_name = format_ident!("__RASTER_SEQUENCE_REGISTRATION_{}", fn_name.to_string().to_uppercase());

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

    let tiles_static_name = format_ident!("__RASTER_SEQUENCE_TILES_{}", fn_name.to_string().to_uppercase());

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
        let names: Vec<_> = params.iter().filter_map(|p| {
            if let FnArg::Typed(pt) = p { Some(&pt.pat) } else { None }
        }).collect();
        let types: Vec<_> = params.iter().filter_map(|p| {
            if let FnArg::Typed(pt) = p { Some(&pt.ty) } else { None }
        }).collect();
        quote! {
            let (#(#names),*): (#(#types),*) = ::raster::parse_main_input()
                .expect("Failed to parse --input argument. Usage: --input '[val1, val2, ...]'");
        }
    };
    
    let expanded = quote! {
        #(#fn_attrs)*
        fn main() {
            if ::raster::try_execute_tile_from_args() {
                return;
            }
            
            #input_parsing
            
            #fn_block
        }
    };
    
    TokenStream::from(expanded)
}

fn add_tracing(input: &ItemFn) -> TokenStream {
    if std::env::var("CARGO_CFG_TARGET_ARCH").map(|v| v == "riscv32").unwrap_or(false) {
        return TokenStream::from(quote! { #input });
    }

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let fn_vis = &input.vis;
    let fn_sig = &input.sig;
    let fn_block = &input.block;
    let fn_attrs = &input.attrs;

    // Create the inner function name
    let inner_fn_name = format_ident!("__{}_impl", fn_name);

    // Get parts of the signature for the inner function
    let fn_generics = &input.sig.generics;
    let fn_inputs = &input.sig.inputs;
    let fn_output = &input.sig.output;

    // Extract argument names to pass to the inner function call
    let arg_names: Vec<_> = input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    return Some(&pat_ident.ident);
                }
            }
            None
        })
        .collect();

    // Generate the trace call code using the same pattern as trace_call!
    // but with the original function name
    let arg_bindings: Vec<_> = arg_names
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            let var_name = format_ident!("__trace_arg_{}", i);
            (var_name, *arg)
        })
        .collect();

    let let_bindings = arg_bindings.iter().map(|(var_name, arg)| {
        quote! { let #var_name = #arg; }
    });

    let call_args = arg_bindings.iter().map(|(var_name, _)| {
        quote! { #var_name }
    });

    let trace_args = arg_bindings.iter().enumerate().map(|(i, (var_name, _))| {
        let arg_name = format!("arg{}", i);
        quote! { (#arg_name, &format!("{:?}", #var_name) as &str) }
    });

    let expanded = quote! {
        #(#fn_attrs)*
        #fn_vis #fn_sig {
            #[inline(always)]
            fn #inner_fn_name #fn_generics (#fn_inputs) #fn_output #fn_block

            #(#let_bindings)*
            let __trace_result = #inner_fn_name(#(#call_args),*);
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            tracing_lite::__emit_trace(
                #fn_name_str,
                &[#(#trace_args),*],
                &format!("{:?}", __trace_result)
            );
            __trace_result
        }
    };

    TokenStream::from(expanded)
}