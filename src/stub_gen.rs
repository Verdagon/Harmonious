use crate::toylang::ast::{Expr, FnBody, Stmt};
use crate::toylang::registry::{ToylangRegistry, ToyFieldType};

pub fn generate(registry: &ToylangRegistry) -> String {
    let mut out = String::new();
    for (name, toy_struct) in &registry.structs {
        if toy_struct.type_params.is_empty() {
            out.push_str(&format!("pub struct {} {{\n", name));
        } else {
            let params = toy_struct.type_params.join(", ");
            out.push_str(&format!("pub struct {}<{}> {{\n", name, params));
        }
        for field in &toy_struct.fields {
            let rust_ty = match &field.rust_type {
                ToyFieldType::I32           => "i32".to_string(),
                ToyFieldType::I64           => "i64".to_string(),
                ToyFieldType::F64           => "f64".to_string(),
                ToyFieldType::Bool          => "bool".to_string(),
                ToyFieldType::TypeParam(p)  => p.clone(),
            };
            out.push_str(&format!("    pub {}: {},\n", field.name, rust_ty));
        }
        out.push_str("}\n");
    }

    // Emit extern "C" declarations for externally-compiled functions.
    // Our LLVM backend matches rustc's ABI coercion (queried via fn_abi_of_instance)
    // so the LLVM-level types agree on both sides.
    let mut has_extern = false;
    for (_name, toy_fn) in &registry.functions {
        if let Some(ref sym) = toy_fn.external_symbol {
            if !has_extern {
                out.push_str("extern \"C\" {\n");
                has_extern = true;
            }
            // Real params from the function signature
            let mut params: Vec<String> = toy_fn.params.iter()
                .map(|p| format!("{}: {}", p.name, p.ty))
                .collect();

            // Add phantom *const () args for Vec deps (must match what
            // build_phantom_call_body generates in mir_helpers.rs)
            let phantom_count = count_vec_deps(toy_fn.body.as_ref());
            for i in 0..phantom_count {
                params.push(format!("_dep{}: *const ()", i));
            }

            let params_str = params.join(", ");
            let ret = toy_fn.return_ty.as_deref().unwrap_or("()");
            out.push_str(&format!("    pub fn {}({}) -> {};\n", sym, params_str, ret));
        }
    }
    if has_extern {
        out.push_str("}\n");
    }

    out
}

/// Count the number of distinct Vec operations in a function body.
/// Must match the dep counting logic in mir_build.rs::collect_rust_deps.
/// Generate #[no_mangle] wrapper functions that provide externally-visible
/// symbols for Rust generic functions. The phantom struct in the MIR stub
/// ensures the underlying generic is monomorphized; the wrapper provides
/// a global symbol the Toylang LLVM code can link against.
fn generate_vec_wrappers(registry: &ToylangRegistry) -> String {
    let mut out = String::new();
    let mut generated = std::collections::HashSet::new();

    for (_name, toy_fn) in &registry.functions {
        if toy_fn.external_symbol.is_none() { continue; }
        let body = match &toy_fn.body {
            Some(b) => b,
            None => continue,
        };

        // Find the element type from the return type or param types
        let ret_ty = toy_fn.return_ty.as_deref().unwrap_or("");
        let elem_name = if ret_ty.starts_with("Vec<") && ret_ty.ends_with('>') {
            Some(&ret_ty[4..ret_ty.len()-1])
        } else {
            // Check params for &Vec<T>
            toy_fn.params.iter().find_map(|p| {
                let t = p.ty.trim_start_matches('&');
                if t.starts_with("Vec<") && t.ends_with('>') {
                    Some(&t[4..t.len()-1])
                } else {
                    None
                }
            })
        };

        let elem_name = match elem_name {
            Some(n) => n.to_string(),
            None => continue,
        };

        if generated.contains(&elem_name) { continue; }

        let mut needs_new = false;
        let mut needs_push = false;
        let mut needs_len = false;
        scan_body(body, &mut needs_new, &mut needs_push, &mut needs_len);

        if needs_new || needs_push || needs_len {
            out.push_str(&format!("// Vec wrappers for {}\n", elem_name));
        }
        if needs_new {
            out.push_str(&format!(
                "#[no_mangle]\npub extern \"C\" fn __toylang_vec_new_{}(out: *mut Vec<{}>) {{\n    unsafe {{ out.write(Vec::new()); }}\n}}\n",
                elem_name, elem_name
            ));
        }
        if needs_push {
            out.push_str(&format!(
                "#[no_mangle]\npub unsafe extern \"C\" fn __toylang_vec_push_{}(v: *mut Vec<{}>, p: *const {}) {{\n    (*v).push(core::ptr::read(p));\n}}\n",
                elem_name, elem_name, elem_name
            ));
        }
        if needs_len {
            out.push_str(&format!(
                "#[no_mangle]\npub unsafe extern \"C\" fn __toylang_vec_len_{}(v: *const Vec<{}>) -> usize {{\n    (*v).len()\n}}\n",
                elem_name, elem_name
            ));
        }

        generated.insert(elem_name);
    }
    out
}

fn count_vec_deps(body: Option<&FnBody>) -> usize {
    let body = match body {
        Some(b) => b,
        None => return 0,
    };
    let mut needs_new = false;
    let mut needs_push = false;
    let mut needs_len = false;
    scan_body(&body, &mut needs_new, &mut needs_push, &mut needs_len);
    (needs_new as usize) + (needs_push as usize) + (needs_len as usize)
}

fn scan_body(body: &FnBody, new: &mut bool, push: &mut bool, len: &mut bool) {
    for stmt in &body.stmts {
        match stmt {
            Stmt::Let { expr, .. } | Stmt::ExprStmt(expr) => scan_expr(expr, new, push, len),
        }
    }
    if let Some(ref ret) = body.ret {
        scan_expr(ret, new, push, len);
    }
}

fn scan_expr(expr: &Expr, new: &mut bool, push: &mut bool, len: &mut bool) {
    match expr {
        Expr::StaticCall { ty, method, .. } if ty == "Vec" && method == "new" => *new = true,
        Expr::MethodCall { receiver, method, .. } if method == "push" => {
            *push = true;
            scan_expr(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, method, .. } if method == "len" => {
            *len = true;
            scan_expr(receiver, new, push, len);
        }
        Expr::MethodCall { receiver, .. } => scan_expr(receiver, new, push, len),
        _ => {}
    }
}
