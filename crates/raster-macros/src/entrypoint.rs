//! Code generation for `main`'s entry arguments.
//!
//! `main` has no caller, so its parameters cannot be supplied as arguments —
//! they name the program's committed inputs. This turns that declaration into
//! the code that binds them: one `bind_entry_arguments` call up front, then
//! one binding per parameter that reads back out of the single combined
//! object it produced. Everything downstream then treats them as ordinary
//! storage values.

use quote::quote;

use crate::ParamInfo;

/// Generates the leading statements that bind `main`'s declared parameters
/// to `main`'s entry arguments: one `bind_entry_arguments` call up front,
/// then one `AuthRef<T>` local per parameter — the same binding type a
/// sequence body sees for its own parameters — carrying `Field(name)` as
/// its selector prefix into the single combined entry object (see
/// `raster_runtime::bind_entry_arguments`). Nested `select!`s compose onto
/// that prefix, so no argument materializes until a tile consumes it.
/// Only meaningful on native (std, non-riscv32) execution — main never
/// actually runs on a tile's riscv32 guest target, but its code still gets
/// compiled there as part of the user crate, so every binding needs a
/// same-typed, unreachable fallback on that path.
fn gen_main_entry_argument_prelude(params: &[ParamInfo]) -> proc_macro2::TokenStream {
    if params.is_empty() {
        return quote! {};
    }

    let spec_exprs: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let ty = &param.ty;
            quote! { ::raster::entry_argument_spec::<#ty>(#name_str) }
        })
        .collect();

    let bind_stmts = quote! {
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        let __raster_entry_binding =
            ::raster::bind_entry_arguments(&[ #(#spec_exprs),* ])
                .expect("Failed to bind main entry arguments");
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        ::raster::publish_trace_event(::raster::core::trace::TraceEvent::EntrypointBind(
            ::raster::core::trace::EntrypointBindEvent {
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
        #bind_stmts
        #(#param_bindings)*
    }
}

/// Prepends `main`'s entry-argument prelude to its body, ahead of the
/// user's own statements — must run after `rewrite_sequence_block` (so the
/// injected calls are never mistaken for tile calls during extraction) and
/// before the body is embedded into the wrapped sequence closure.
pub(crate) fn prepend_entry_argument_prelude(block: &mut syn::Block, params: &[ParamInfo]) {
    if params.is_empty() {
        return;
    }
    let prelude_tokens = gen_main_entry_argument_prelude(params);
    let prelude_block: syn::Block = syn::parse2(quote! { { #prelude_tokens } })
        .expect("Failed to parse generated main entry-argument prelude");
    let mut new_stmts = prelude_block.stmts;
    new_stmts.append(&mut block.stmts);
    block.stmts = new_stmts;
}
