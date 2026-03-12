use cargo_toml::Manifest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::{
    parse_file, visit::Visit, Attribute, Expr, ExprLit, ExprMacro, FnArg, Lit, Local, Meta, Pat,
    StmtMacro,
};
use walkdir::WalkDir;

use raster_core::Result;

#[derive(Debug, Clone)]
pub struct ProjectAst {
    pub name: String,
    pub root_path: PathBuf,
    pub functions: Vec<FunctionAstItem>,
}

/// Indicates whether a call was made via `call!` (tile) or `call_seq!` (sequence).
///
/// Only canonical call primitives are recognized by the compiler. Bare function calls
/// in sequence bodies are not extracted — authors must use `call!` or `call_seq!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    /// Invoked via `call!(tile_fn, args...)` — caller declares this is a tile step.
    Tile,
    /// Invoked via `call_seq!(seq_fn, args...)` — caller declares this is a sequence call.
    Sequence,
}

/// Captures detailed information about a function call within a function body.
#[derive(Debug, Clone)]
pub struct CallInfo {
    /// The name of the function being called
    pub callee: String,
    /// The arguments passed to the function (as string representations)
    pub arguments: Vec<String>,
    /// The variable name this call result is bound to, if any (e.g., `let x = foo()` -> Some("x"))
    pub result_binding: Option<String>,
    /// How the call was made: via `call!`, `call_seq!`, or a bare function call.
    pub call_kind: CallKind,
}

#[derive(Debug, Clone)]
pub struct FunctionAstItem {
    pub name: String,
    pub path: PathBuf,
    /// Detailed information about each function call in this function's body
    pub call_infos: Vec<CallInfo>,
    pub macros: Vec<MacroAstItem>,
    /// Parameter names of the function
    pub input_names: Vec<String>,
    /// Parameter types of the function
    pub inputs: Vec<String>,
    pub output: Option<String>,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct MacroAstItem {
    pub name: String,
    pub args: HashMap<String, String>,
}
// TODO: Project AST Explorer should contain function with full path resulution (like mod's)
impl ProjectAst {
    pub fn new(project_root: &Path) -> Result<Self> {
        let cargo_toml_path = project_root.join("Cargo.toml");

        let manifest = Manifest::from_path(&cargo_toml_path).unwrap();

        let package = manifest.package.as_ref().unwrap();

        let files_paths = Self::find_all_rs_files(project_root);

        let functions = files_paths
            .iter()
            .flat_map(|path| Self::parse_file(&path))
            .collect::<Vec<FunctionAstItem>>();

        Ok(Self {
            name: package.name.clone(),
            root_path: project_root.to_path_buf(),
            functions,
        })
    }

    fn parse_file(path: &Path) -> Vec<FunctionAstItem> {
        let content = std::fs::read_to_string(path).unwrap();
        let ast = parse_file(&content).unwrap();
        let functions = Self::parse_functions(&ast, path.to_path_buf());

        functions
    }

    fn find_all_rs_files(project_root: &Path) -> Vec<PathBuf> {
        let src_dir = project_root.join("src");

        WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "rs"))
            .filter(|e| e.path().file_name().unwrap_or_default() != "mod.rs")
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    fn parse_functions(ast: &syn::File, path: PathBuf) -> Vec<FunctionAstItem> {
        let mut functions = Vec::new();

        for item in &ast.items {
            if let syn::Item::Fn(func) = item {
                let name = func.sig.ident.to_string();

                let macros: Vec<MacroAstItem> = func
                    .attrs
                    .iter()
                    .filter(|attr| !attr.path().is_ident("doc")) // Skip doc attributes
                    .map(|attr| Self::parse_macro(attr))
                    .collect();

                let (input_names, inputs): (Vec<String>, Vec<String>) = func
                    .sig
                    .inputs
                    .iter()
                    .filter_map(|arg| {
                        if let FnArg::Typed(pat_type) = arg {
                            let ty = &pat_type.ty;
                            let name = Self::extract_param_name(&pat_type.pat);
                            Some((name, quote::quote!(#ty).to_string()))
                        } else {
                            None
                        }
                    })
                    .unzip();

                let output = if let syn::ReturnType::Type(_, ty) = &func.sig.output {
                    Some(quote::quote!(#ty).to_string())
                } else {
                    None
                };

                let sig = &func.sig;
                let signature = quote::quote!(#sig).to_string();

                let mut visitor = CallVisitor::new();
                visitor.visit_item_fn(func);
                let call_infos = visitor.get_call_infos();
                let function_info = FunctionAstItem {
                    name,
                    path: path.clone(),
                    call_infos,
                    macros,
                    input_names,
                    inputs,
                    output,
                    signature,
                };
                functions.push(function_info);
            }
        }
        functions
    }

    fn parse_macro(attr: &Attribute) -> MacroAstItem {
        let name = attr
            .path()
            .segments
            .iter()
            .map(|seg| seg.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");

        let mut args = HashMap::new();

        if let Meta::List(meta_list) = &attr.meta {
            let _ = meta_list.parse_nested_meta(|meta| {
                let key = meta
                    .path
                    .get_ident()
                    .map(|id| id.to_string())
                    .unwrap_or_default();

                let value: Expr = meta.value()?.parse()?;

                // Convert everything to String
                let value_str = match &value {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) => s.value(),
                    Expr::Lit(ExprLit {
                        lit: Lit::Int(i), ..
                    }) => i.to_string(),
                    Expr::Lit(ExprLit {
                        lit: Lit::Bool(b), ..
                    }) => b.value.to_string(),
                    Expr::Lit(ExprLit {
                        lit: Lit::Float(f), ..
                    }) => f.to_string(),
                    _ => quote::quote!(#value).to_string(),
                };

                args.insert(key, value_str);
                Ok(())
            });
        }

        MacroAstItem { name, args }
    }

    /// Extracts the parameter name from a pattern
    fn extract_param_name(pat: &Pat) -> String {
        match pat {
            Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
            Pat::Type(pat_type) => Self::extract_param_name(&pat_type.pat),
            Pat::Wild(_) => "_".to_string(),
            _ => quote::quote!(#pat).to_string(),
        }
    }
}

pub struct CallVisitor {
    call_infos: Vec<CallInfo>,
    /// Tracks the current let binding name when visiting let statements
    current_binding: Option<String>,
}

impl CallVisitor {
    fn new() -> Self {
        Self {
            call_infos: Vec::new(),
            current_binding: None,
        }
    }

    fn get_call_infos(&self) -> Vec<CallInfo> {
        self.call_infos.clone()
    }

    /// Extracts the binding name from a pattern (e.g., `x` from `let x = ...`)
    fn extract_binding_name(pat: &Pat) -> Option<String> {
        match pat {
            Pat::Ident(pat_ident) => Some(pat_ident.ident.to_string()),
            Pat::Type(pat_type) => Self::extract_binding_name(&pat_type.pat),
            _ => None,
        }
    }

    /// Converts an expression to its string representation for argument capture
    fn expr_to_string(expr: &Expr) -> String {
        quote::quote!(#expr).to_string()
    }

    /// Parse a `call!` or `call_seq!` macro token stream into (callee, arguments).
    ///
    /// The macro syntax is `call!(callee_fn, arg1, arg2, ...)`. The token stream
    /// is a comma-separated list; the first token is the callee identifier.
    fn parse_call_macro_args(mac: &syn::Macro) -> Option<(String, Vec<String>)> {
        // Parse the macro tokens as a punctuated sequence of expressions.
        let args: syn::punctuated::Punctuated<Expr, syn::Token![,]> = mac
            .parse_body_with(syn::punctuated::Punctuated::parse_terminated)
            .ok()?;

        let mut iter = args.iter();

        // First argument must be a plain identifier (the callee function name).
        let callee_expr = iter.next()?;
        let callee = match callee_expr {
            Expr::Path(path) => path.path.get_ident()?.to_string(),
            _ => return None,
        };

        // Remaining arguments are the call arguments.
        let arguments: Vec<String> = iter.map(Self::expr_to_string).collect();

        Some((callee, arguments))
    }

    /// Check if a macro path matches one of the canonical call primitive names.
    ///
    /// Matches: `call`, `call_seq`, `raster::call`, `raster::call_seq`.
    fn macro_call_kind(mac: &syn::Macro) -> Option<CallKind> {
        let segments: Vec<String> = mac
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();

        match segments.as_slice() {
            [name] if name == "call" => Some(CallKind::Tile),
            [prefix, name] if prefix == "raster" && name == "call" => Some(CallKind::Tile),
            [name] if name == "call_seq" => Some(CallKind::Sequence),
            [prefix, name] if prefix == "raster" && name == "call_seq" => Some(CallKind::Sequence),
            _ => None,
        }
    }
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_local(&mut self, node: &'ast Local) {
        // Extract the binding name from the let pattern
        let binding_name = Self::extract_binding_name(&node.pat);

        // Set the current binding context before visiting the initializer
        self.current_binding = binding_name;

        // Visit the initializer expression (this will trigger visit_expr_call /
        // visit_expr_macro if there's a call or macro invocation)
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
        }

        // Clear the binding context after processing
        self.current_binding = None;
    }

    fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
        if let Some(call_kind) = Self::macro_call_kind(&node.mac) {
            if let Some((callee, arguments)) = Self::parse_call_macro_args(&node.mac) {
                let result_binding = self.current_binding.take();
                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    result_binding,
                    call_kind,
                });
                // Do not recurse into the macro body — arguments are already captured above.
                return;
            }
        }
        // For unrecognized macros, continue default visitation.
        syn::visit::visit_expr_macro(self, node);
    }

    fn visit_stmt_macro(&mut self, node: &'ast StmtMacro) {
        // Bare macro statements (e.g. `call_seq!(foo, x);` without a `let` binding) are
        // parsed as `Stmt::Macro(StmtMacro)` by syn, not as `Stmt::Expr(Expr::Macro)`.
        // The default Visit path for StmtMacro calls `visit_macro` (the raw Macro struct),
        // which does NOT trigger `visit_expr_macro`. We handle them here so that statement-
        // position `call!` and `call_seq!` invocations are captured without a binding.
        if let Some(call_kind) = Self::macro_call_kind(&node.mac) {
            if let Some((callee, arguments)) = Self::parse_call_macro_args(&node.mac) {
                // current_binding is None here — bare statements have no let binding.
                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    result_binding: None,
                    call_kind,
                });
                return;
            }
        }
        syn::visit::visit_stmt_macro(self, node);
    }

    // Note: visit_expr_call is intentionally NOT overridden. Bare function calls
    // (e.g. `greet(name)`) are not extracted — only canonical `call!` and `call_seq!`
    // macro invocations are recognized as step boundaries in sequences.
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::visit::Visit;

    fn parse_calls(code: &str) -> Vec<CallInfo> {
        let file: syn::File = syn::parse_str(code).expect("Failed to parse test code");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);
        visitor.get_call_infos()
    }

    #[test]
    fn test_bare_call_not_extracted() {
        // Bare function calls (without call!/call_seq!) must NOT be extracted.
        let calls = parse_calls("fn seq() { let x = greet(name); }");
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn test_call_macro_extraction() {
        let calls = parse_calls("fn seq() { let greeting = call!(greet, name); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "greet");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("greeting"));
        assert_eq!(calls[0].arguments, vec!["name"]);
    }

    #[test]
    fn test_call_seq_macro_extraction() {
        let calls = parse_calls("fn seq() { let result = call_seq!(wish_sequence, greeting); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "wish_sequence");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding.as_deref(), Some("result"));
        assert_eq!(calls[0].arguments, vec!["greeting"]);
    }

    #[test]
    fn test_call_macro_no_args() {
        let calls = parse_calls("fn seq() { let r = call!(no_arg_tile); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "no_arg_tile");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].arguments.len(), 0);
    }

    #[test]
    fn test_call_macro_multiple_args() {
        let calls = parse_calls("fn seq() { let r = call!(tile_fn, a, b, c); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "tile_fn");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].arguments.len(), 3);
    }

    #[test]
    fn test_mixed_calls_in_sequence() {
        let calls = parse_calls(
            r#"
            fn seq(name: String) -> String {
                let greeting = call!(greet, name);
                let e1 = call!(exclaim, greeting);
                let result = call_seq!(wish_sequence, e1);
                result
            }
            "#,
        );
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].callee, "greet");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[1].callee, "exclaim");
        assert_eq!(calls[1].call_kind, CallKind::Tile);
        assert_eq!(calls[2].callee, "wish_sequence");
        assert_eq!(calls[2].call_kind, CallKind::Sequence);
    }

    #[test]
    fn test_statement_level_call_seq_no_binding() {
        // call_seq! as a bare statement (no `let`) must still be captured.
        let calls = parse_calls(
            r#"
            fn main() {
                call_seq!(greet_sequence, x);
            }
            "#,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "greet_sequence");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding, None);
    }

    #[test]
    fn test_statement_level_call_seq_extraction() {
        // call_seq! used as a statement expression (no `let` binding) must still be captured.
        let calls = parse_calls(
            r#"
            fn main(name: String) {
                call_seq!(greet_sequence, "Rust".to_string());
                let name_2 = call_seq!(placeholder_sequence, name);
                let _result = call_seq!(greet_sequence, name_2);
            }
            "#,
        );
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].callee, "greet_sequence");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding, None);
        assert_eq!(calls[1].callee, "placeholder_sequence");
        assert_eq!(calls[1].call_kind, CallKind::Sequence);
        assert_eq!(calls[1].result_binding.as_deref(), Some("name_2"));
        assert_eq!(calls[2].callee, "greet_sequence");
        assert_eq!(calls[2].call_kind, CallKind::Sequence);
        assert_eq!(calls[2].result_binding.as_deref(), Some("_result"));
    }

    #[test]
    fn test_qualified_call_macro_extraction() {
        let calls = parse_calls("fn seq() { let x = raster::call!(greet, name); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "greet");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("x"));
    }

    #[test]
    fn test_qualified_call_seq_macro_extraction() {
        let calls = parse_calls("fn seq() { let x = raster::call_seq!(wish_seq, name); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "wish_seq");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding.as_deref(), Some("x"));
    }
}
