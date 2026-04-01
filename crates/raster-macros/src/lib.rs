//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow. When the function is named `main`,
//!   it is the program entry point and gets init, `--input` parsing, and finish automatically.

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType, Type};

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
                .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let tuple_type = quote! { (#(#input_types),*) };
        quote! {
            let (#(#input_names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    }
}

/// Add output serialization to the code pipeline.
///
/// Wraps the provided code with serialization of `result` to `output`.
fn gen_output_serialization(_input: &ItemFn) -> proc_macro2::TokenStream {
    quote! {
        let output = ::raster::core::postcard::to_allocvec(&result)
            .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to serialize output: {}", e)))?;
    }
}

/// Generate input serialization code for tracing in the original function.
///
/// This serializes the typed input parameters to bytes for the trace emission.
fn gen_input_serialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let (input_types, input_names) = extract_inputs(input);

    let input_param_tuples: Vec<_> = input_names
        .iter()
        .zip(input_types.iter())
        .map(|(name, ty)| {
            let name_str = name.to_string();
            let ty_str = ty.to_token_stream().to_string();
            quote! { (#name_str, #ty_str) }
        })
        .collect();

    let input_bytes = if input_types.is_empty() {
        quote! {
            let __raster_input_bytes: ::raster::alloc::vec::Vec<u8> = ::raster::alloc::vec::Vec::new();
        }
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
    };

    quote! {
        let __raster_input_args: ::raster::alloc::vec::Vec<::raster::core::trace::FnInputArgs> = [
                #(#input_param_tuples),*
            ]
                .iter()
                .map(|(n, t)| ::raster::core::trace::FnInputArgs {
                    name: ::raster::alloc::string::String::from(*n),
                    ty: ::raster::alloc::string::String::from(*t),
                })
                .collect();

        #input_bytes

        let __raster_input = ::core::option::Option::Some(::raster::core::trace::FnInput {
            data: __raster_input_bytes,
            args: __raster_input_args,
        });
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

    let function_wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name_str);

    let attrs = TileAttrs::parse(attr);

    // Generate optional metadata fields
    let _estimated_cycles_expr = match attrs.estimated_cycles {
        Some(cycles) => quote! { ::core::option::Option::Some(#cycles) },
        None => quote! { ::core::option::Option::None },
    };

    let _max_memory_expr = match attrs.max_memory {
        Some(memory) => quote! { ::core::option::Option::Some(#memory) },
        None => quote! { ::core::option::Option::None },
    };

    // Generate deserialization and function call
    let inputs_deserialization = gen_inputs_deserialization(&input_fn);
    let function_call = gen_function_call(&input_fn);
    let output_serialization = gen_output_serialization(&input_fn);

    // For recursive tiles, also generate a macro with the same name that allows `tile_name!(args)` syntax
    let _recursive_macro = if attrs.tile_type == "recur" {
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

    // Generate output type expression
    let output_type_expr = match &input_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    // Generate input serialization code for tracing
    let input_serialization = gen_input_serialization(&input_fn);

    let original_function = quote! {
        #(#fn_attrs)*
        #fn_vis #fn_sig {
            // On std + non-riscv32: wrap body in closure for tracing
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                // Serialize inputs for tracing
                #input_serialization

                // Execute original body via closure to handle early returns
                let __raster_result = (|| #fn_body)();

                // Serialize output and emit TraceEvent::TileExec
                let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_result)
                    .unwrap_or_default();

                let __raster_output = ::core::option::Option::Some(
                    ::raster::core::trace::FnOutput {
                        data: __raster_output_bytes,
                        ty: ::alloc::string::String::from(#output_type_expr),
                    }
                );

                let __raster_record = ::raster::core::trace::FnCallRecord {
                    fn_name: ::alloc::string::String::from(#fn_name_str),
                    input: __raster_input,
                    output: __raster_output,
                };
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::TileExec(
                    __raster_record,
                ));

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
        pub fn #function_wrapper_name(input: &[u8]) -> ::raster::core::Result<::raster::alloc::vec::Vec<u8>> {
            #inputs_deserialization

            #function_call

            #output_serialization

            Ok(output)
        }
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

/// Generates the sequence-wrapped body: either tracing (SequenceStart/body/SequenceEnd) or plain body depending on cfg.
fn gen_sequence_wrapped_body(
    fn_name_str: &str,
    item_fn: &ItemFn,
) -> proc_macro2::TokenStream {
    let body = &item_fn.block;
    let input_serialization = gen_input_serialization(&item_fn);

    let output_type_expr = match &item_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    quote! {
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        {
            #input_serialization

            let mut __raster_record = ::raster::core::trace::FnCallRecord {
                fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                input: __raster_input,
                output: ::core::option::Option::None,
            };
            ::raster::publish_trace_event(::raster::core::trace::TraceEvent::SequenceStart(
                __raster_record.clone(),
            ));
            let __raster_result = (|| #body)();
            let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_result)
                .unwrap_or_default();
            __raster_record.output = ::core::option::Option::Some(::raster::core::trace::FnOutput::new(
                __raster_output_bytes,
                ::raster::alloc::string::String::from(#output_type_expr),
            ));
            ::raster::publish_trace_event(::raster::core::trace::TraceEvent::SequenceEnd(
                __raster_record,
            ));
            __raster_result
        }

        #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
        #body
    }
}

/// Declares a sequence of tiles with linear control flow.
///
/// The `#[sequence]` macro parses the function body to extract tile calls
/// in the order they appear. The function remains callable for native execution,
/// and the sequence is registered for use with `cargo raster preview`.
///
/// When the function is named **`main`**, it is the program entry point: the macro expands to
/// `fn main() { init(); input_parsing; sequence_wrapped_body; finish(); }`. The `--input` CLI
/// argument is parsed into the original function's parameters.
///
/// # Attributes
/// - `description = "..."` - Human-readable description of the sequence
///
/// # Example (entry point)
/// ```ignore
/// #[raster::sequence]
/// fn main(name: String) {
///     let result = greet_sequence(name);
///     println!("{}", result);
/// }
/// ```
///
/// # Example (nested sequence)
/// ```ignore
/// #[sequence]
/// fn greet_sequence(name: String) -> String {
///     exclaim(greet(name))
/// }
/// ```
#[proc_macro_attribute]
pub fn sequence(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_fn = parse_macro_input!(item as ItemFn);
    let fn_name_str = item_fn.sig.ident.to_string();

    let fn_vis = &item_fn.vis;
    let fn_sig = &item_fn.sig;
    let fn_attrs = &item_fn.attrs;
    let _attrs = SequenceAttrs::parse(attr);


    let expanded = if item_fn.sig.ident == "main" {
        // Entry point: replace with fn main() { init(); input_parsing; wrapped_body; finish(); }
        let params: Vec<_> = item_fn.sig.inputs.iter().collect();
        let input_parsing = if params.is_empty() {
            quote! {}
        } else if params.len() == 1 {
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
        let body = gen_sequence_wrapped_body("main",  &item_fn);
        quote! {
            #(#fn_attrs)*
            fn main() {
                ::raster::init();

                #input_parsing

                #body

                ::raster::finish();
            }
        }
    } else {
        // Normal sequence: keep signature, wrap body
        let body = gen_sequence_wrapped_body(&fn_name_str, &item_fn);
        quote! {
            #(#fn_attrs)*
            #fn_vis #fn_sig {
                #body
            }
        }
    };

    TokenStream::from(expanded)
}
