use anyhow::Result;
use cargo_toml::Manifest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::{
    parse_file,
    visit::Visit,
    Attribute, Expr, ExprCall, ExprLit, FnArg, Lit, Local, Meta, Pat,
};
use walkdir::WalkDir;


#[derive(Debug, Clone)]
pub struct ProjectAst {
    pub name: String,
    pub root_path: PathBuf,
    pub functions: Vec<FunctionAstItem>,
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
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_local(&mut self, node: &'ast Local) {
        // Extract the binding name from the let pattern
        let binding_name = Self::extract_binding_name(&node.pat);
        
        // Set the current binding context before visiting the initializer
        self.current_binding = binding_name;
        
        // Visit the initializer expression (this will trigger visit_expr_call if there's a call)
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
        }
        
        // Clear the binding context after processing
        self.current_binding = None;
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = &*node.func {
            if let Some(ident) = path.path.get_ident() {
                let callee = ident.to_string();

                // Capture all arguments as string representations
                let arguments: Vec<String> = node
                    .args
                    .iter()
                    .map(|arg| Self::expr_to_string(arg))
                    .collect();

                // Take the current binding if this is a direct call in a let statement
                let result_binding = self.current_binding.take();

                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    result_binding,
                });
            }
        }
        
        // Continue visiting nested calls (but clear binding context for nested calls)
        let saved_binding = self.current_binding.take();
        syn::visit::visit_expr_call(self, node);
        self.current_binding = saved_binding;
    }
}
