//! Facade-decoupling fence (Phase 6 of the Approach B work).
//!
//! Phase 6 (commit `4854a5a`) decoupled the facade from Inkwell and
//! `rustc_codegen_llvm::ModuleLlvm`. The `LangCallbacks::consumer_fill_modules`
//! trait method now takes `&mut rustc_lang_facade::LlvmModuleFactory`, which
//! hands consumers raw `LLVMContextRef` / `LLVMModuleRef` pointers via
//! [`BorrowedLlvmModule`]. Consumers wrap with whichever LLVM API they prefer
//! (Inkwell, `llvm-sys`, C++ via FFI) — toylang chose Inkwell internally.
//!
//! This fence guards the decoupling: a future refactor must not re-introduce
//! a direct reference to rustc's codegen crates or to facade-internal types
//! that the LLVM-API-agnostic surface deliberately hides. The Sky-C++-via-FFI
//! path depends on the facade staying neutral.
//!
//! Banned identifiers in toylang source code (not in comments):
//!   - `rustc_codegen_llvm` — facade-internal; consumers go through the
//!     facade's `LlvmModuleFactory` / `BorrowedLlvmModule`.
//!   - `rustc_codegen_ssa`  — same.
//!   - `ModuleLlvm`         — facade-internal type the consumer never sees.
//!   - `ExtraModuleAllocator` — same; facade wraps it in `LlvmModuleFactory`.
//!
//! Doc comments and inline comments are permitted to mention these terms for
//! explanatory purposes — the scan strips comments before checking.

use std::fs;
use std::path::{Path, PathBuf};

/// Strip Rust comments (line `//...`, block `/* ... */`) from a source string.
/// Returns the code-only text. Naive but sufficient for fence checks: does
/// not need to handle string-literal escapes precisely because the banned
/// identifiers are unlikely to ever appear inside string literals.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut in_block = false;
    while i < bytes.len() {
        if in_block {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
            } else {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // Line comment — skip to next newline.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            in_block = true;
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Walk a directory tree and return every `*.rs` file under it.
fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn toylang_source_has_no_rustc_codegen_coupling() {
    let banned: &[(&str, &str)] = &[
        (
            "rustc_codegen_llvm",
            "rustc_codegen_llvm is facade-internal; consumers see only \
             rustc_lang_facade::BorrowedLlvmModule / LlvmModuleFactory",
        ),
        (
            "rustc_codegen_ssa",
            "rustc_codegen_ssa is facade-internal; consumers see only \
             rustc_lang_facade::BorrowedLlvmModule / LlvmModuleFactory",
        ),
        (
            "ModuleLlvm",
            "ModuleLlvm is facade-internal; consumers see BorrowedLlvmModule's \
             raw context/module pointers and wrap with their chosen LLVM API",
        ),
        (
            "ExtraModuleAllocator",
            "ExtraModuleAllocator is facade-internal; consumers go through \
             LlvmModuleFactory::fill_module",
        ),
    ];

    let src_root = Path::new("src");
    assert!(
        src_root.exists(),
        "fence test must run from the toylangc crate root (looking for ./src)"
    );

    let mut rs_files = Vec::new();
    collect_rs_files(src_root, &mut rs_files);
    rs_files.sort();
    assert!(
        !rs_files.is_empty(),
        "no .rs files found under ./src — fence cannot validate"
    );

    let mut violations: Vec<String> = Vec::new();
    for path in &rs_files {
        let src = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
        let code_only = strip_comments(&src);
        // Build a line-index map so we can report the original line number
        // even though strip_comments preserves newlines.
        for (lineno, line) in code_only.lines().enumerate() {
            for (banned_id, _) in banned {
                if line.contains(banned_id) {
                    violations.push(format!(
                        "{}:{}: {}: {}",
                        path.display(),
                        lineno + 1,
                        banned_id,
                        line.trim()
                    ));
                }
            }
        }
    }

    if !violations.is_empty() {
        let mut reasons = String::new();
        for (id, why) in banned {
            reasons.push_str(&format!("  - {}: {}\n", id, why));
        }
        panic!(
            "Facade-decoupling fence (Phase 6 of Approach B): toylang source \
             may not reference facade-internal types directly. The facade \
             gives consumers an LLVM-API-agnostic surface\n\
             (BorrowedLlvmModule + LlvmModuleFactory); breaking that coupling \
             closes off Sky's planned C++-via-FFI codegen path.\n\n\
             Banned identifiers and why:\n{}\n\
             Violations (in code, not in comments):\n  {}",
            reasons,
            violations.join("\n  ")
        );
    }
}
