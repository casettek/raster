//! Code generation for `main`'s program start and entry arguments.
//!
//! `main` has no caller, so its parameters cannot be supplied as arguments —
//! they name the program's committed inputs. Every `main` opens with a single
//! `start_program` call: it publishes the program's first trace event
//! (`TraceEvent::ProgramStart`) and, when `main` declares parameters, binds
//! them into one authorized storage object. Each parameter then reads back
//! out of that object as an ordinary `AuthRef<T>`, and everything downstream
//! treats it as an ordinary storage value.

use quote::quote;

use crate::ParamInfo;

/// Generates the leading statements that start the program and bind `main`'s
/// declared parameters to its entry arguments: one `start_program` call up
/// front (publishing `TraceEvent::ProgramStart`), then one `AuthRef<T>` local
/// per parameter — the same binding type a sequence body sees for its own
/// parameters — carrying `Field(name)` as its selector prefix into the single
/// combined entry object (see `raster_runtime::start_program`). Nested
/// `select!`s compose onto that prefix, so no argument materializes until a
/// tile consumes it.
///
/// Emitted for every `main`, even one with no parameters (which starts the
/// program and binds nothing). Only meaningful on native (std, non-riscv32)
/// execution — main never actually runs on a tile's riscv32 guest target, but
/// its code still gets compiled there as part of the user crate, so every
/// binding needs a same-typed, unreachable fallback on that path.
fn gen_main_entry_argument_prelude(params: &[ParamInfo]) -> proc_macro2::TokenStream {
    let spec_exprs: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let ty = &param.ty;
            quote! { ::raster::entry_argument_spec::<#ty>(#name_str) }
        })
        .collect();

    // A program with no declared parameters still starts (and publishes its
    // `ProgramStart` event); the binding it returns is unused, so scope it to
    // a block rather than leaking a dead local.
    if params.is_empty() {
        return quote! {
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                let __raster_entry_binding =
                    ::raster::start_program(&[]).expect("Failed to start program");
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::ProgramStart(
                    ::raster::core::trace::ProgramStartEvent {
                        arguments: __raster_entry_binding.arguments,
                    },
                ));
            }
        };
    }

    let start_stmts = quote! {
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        let __raster_entry_binding =
            ::raster::start_program(&[ #(#spec_exprs),* ])
                .expect("Failed to start program");
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        ::raster::publish_trace_event(::raster::core::trace::TraceEvent::ProgramStart(
            ::raster::core::trace::ProgramStartEvent {
                arguments: __raster_entry_binding.arguments.clone(),
            },
        ));
    };

    let param_bindings: Vec<_> = params
        .iter()
        .map(|param| {
            let ident = &param.ident;
            let ty = &param.ty;
            let name_str = ident.to_string();
            quote! {
                #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
                let #ident: ::raster::AuthRef<#ty> =
                    ::raster::entry_argument_auth_ref::<#ty>(
                        __raster_entry_binding.reference.clone(),
                        #name_str,
                    );
                #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
                let #ident: ::raster::AuthRef<#ty> = ::core::unreachable!(
                    "main entry arguments are only bound on native (std, non-riscv32) execution"
                );
            }
        })
        .collect();

    quote! {
        #start_stmts
        #(#param_bindings)*
    }
}

/// Prepends `main`'s program-start prelude to its body, ahead of the user's
/// own statements — must run after `rewrite_sequence_block` (so the injected
/// calls are never mistaken for tile calls during extraction) and before the
/// body is embedded into the wrapped sequence closure. Always injects, even
/// when `main` declares no parameters, so every program publishes a
/// `ProgramStart` event.
pub(crate) fn prepend_entry_argument_prelude(block: &mut syn::Block, params: &[ParamInfo]) {
    let prelude_tokens = gen_main_entry_argument_prelude(params);
    let prelude_block: syn::Block = syn::parse2(quote! { { #prelude_tokens } })
        .expect("Failed to parse generated main program-start prelude");
    let mut new_stmts = prelude_block.stmts;
    new_stmts.append(&mut block.stmts);
    block.stmts = new_stmts;
}
