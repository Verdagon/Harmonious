//! Disassembly harness for inlining-matrix tests.
//!
//! Lifts and generalizes the in-line `assert_sky_inlined_into_main`
//! logic so the ~49-fixture inlining matrix can assert per-fn / per-
//! callee inlining behavior uniformly. Used from
//! `integration_projects.rs` via:
//!
//!   #[path = "common/inlining_harness.rs"]
//!   mod inlining_harness;
//!
//! `llvm-objdump` is located via `$LLVM_SYS_211_PREFIX/bin/llvm-objdump`.
//! Hard-fail when the tool or the binary is missing — silent skips
//! defeat the architectural assertion.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Cargo profile dir under the shared `CARGO_TARGET_DIR`. Toylang
/// builds always emit to `debug/` regardless of the toml's `opt-level`
/// because the [profile.dev] override mechanism in build.rs stays on
/// debug-profile (see manifest.rs / build.rs comments). `Release` is
/// here for forward-compat — current toylang never produces it.
#[allow(dead_code)]
pub enum Profile {
    Debug,
    Release,
}

/// Parsed `llvm-objdump -d` output. `functions` maps a *demangled*
/// substring → the list of disassembly lines inside that function's
/// body (excluding the header line itself). Callers grep with
/// `contains` on the demangled key, so the same lookup works whether
/// the symbol is mangled or already demangled in objdump's output.
pub struct DisasmContext {
    pub project_name: String,
    pub bin_path: PathBuf,
    /// `(demangled_function_name, body_lines)` in source order.
    /// `Vec` (not `HashMap`) because callers often want to scan ALL
    /// functions matching a substring, not just one.
    pub functions: Vec<(String, Vec<String>)>,
}

impl DisasmContext {
    /// All function bodies whose demangled name contains `needle`.
    pub fn bodies_of<'a>(&'a self, needle: &str) -> Vec<&'a [String]> {
        self.functions
            .iter()
            .filter(|(name, _)| name.contains(needle))
            .map(|(_, body)| body.as_slice())
            .collect()
    }

    /// True iff the binary disassembly contains any function whose
    /// demangled name contains `needle`.
    #[allow(dead_code)]
    pub fn has_function(&self, needle: &str) -> bool {
        self.functions.iter().any(|(name, _)| name.contains(needle))
    }
}

/// Run `llvm-objdump -d --no-show-raw-insn` on the project's binary
/// and parse the result. Each `bl`/`call`/`b ` target's mangled
/// symbol is demangled in place so callers can assert against
/// human-readable substrings (e.g. `"lto_smoke::main"`,
/// `"some_rust_lib::duplicate"`).
pub fn disassemble_binary(project_name: &str, profile: Profile) -> DisasmContext {
    let cargo_target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/integration-projects-cache");
    let subdir = match profile {
        Profile::Debug => "debug",
        Profile::Release => "release",
    };
    let bin = cargo_target.join(subdir).join(project_name);
    assert!(
        bin.exists(),
        "{}: binary not found at {}",
        project_name,
        bin.display(),
    );

    let llvm_prefix = std::env::var("LLVM_SYS_211_PREFIX").expect(
        "LLVM_SYS_211_PREFIX env not set — the test harness sets it; \
         if running inlining tests manually you must too",
    );
    let objdump = PathBuf::from(&llvm_prefix).join("bin").join("llvm-objdump");
    assert!(
        objdump.exists(),
        "llvm-objdump not found at {} (expected from LLVM_SYS_211_PREFIX)",
        objdump.display(),
    );

    let dump = Command::new(&objdump)
        .args(["-d", "--no-show-raw-insn"])
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("{}: failed to run llvm-objdump: {}", project_name, e));
    assert!(
        dump.status.success(),
        "{}: llvm-objdump failed:\nstdout: {}\nstderr: {}",
        project_name,
        String::from_utf8_lossy(&dump.stdout),
        String::from_utf8_lossy(&dump.stderr),
    );
    let asm = String::from_utf8_lossy(&dump.stdout);

    let mut functions: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in asm.lines() {
        if is_function_header(line) {
            if let Some(prev) = current.take() {
                functions.push(prev);
            }
            let demangled = extract_and_demangle_header(line);
            current = Some((demangled, Vec::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            // Demangle any `<...mangled...>` annotation in the line so
            // callers can substring-match on the demangled name.
            let demangled_line = demangle_inline_refs(line);
            body.push(demangled_line);
        }
    }
    if let Some(prev) = current.take() {
        functions.push(prev);
    }

    DisasmContext {
        project_name: project_name.to_string(),
        bin_path: bin,
        functions,
    }
}

fn is_function_header(line: &str) -> bool {
    // llvm-objdump function headers look like `0000000100000123 <symbol>:`.
    // Cheap check: ends with `:` AND contains `<`.
    line.ends_with(':') && line.contains('<')
}

fn extract_and_demangle_header(line: &str) -> String {
    // Header is `<addr> <symbol>:`. Extract the contents of the
    // outermost angle brackets, then demangle.
    let start = line.find('<');
    let end = line.rfind('>');
    if let (Some(s), Some(e)) = (start, end) {
        if e > s {
            let raw = &line[s + 1..e];
            return format!("{:#}", rustc_demangle::demangle(raw));
        }
    }
    line.to_string()
}

fn demangle_inline_refs(line: &str) -> String {
    // Instruction lines can carry `<mangled_symbol>` annotations after
    // the target address: `bl  0x100002bb4 <__RNvCsxxx_...>`. Replace
    // each such annotation with its demangled form so callers can
    // match on readable substrings.
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find('>') {
            let raw = &after_open[..close];
            // Only demangle if it looks vaguely like a rustc symbol
            // (contains a `:` or starts with `_R` / `_Z` / `__R`).
            if raw.starts_with("_R") || raw.starts_with("_Z") || raw.starts_with("__R")
                || raw.contains("::")
            {
                let demangled = format!("{:#}", rustc_demangle::demangle(raw));
                out.push('<');
                out.push_str(&demangled);
                out.push('>');
            } else {
                out.push('<');
                out.push_str(raw);
                out.push('>');
            }
            rest = &after_open[close + 1..];
        } else {
            out.push('<');
            out.push_str(after_open);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Extract the instruction mnemonic from a disasm line. objdump emits
/// lines like `<addr>:    <ws> <mnemonic> <ws> <operands>`. Returns the
/// mnemonic, or None if the line has no colon (not a disasm line).
fn extract_mnemonic(line: &str) -> Option<&str> {
    let after_colon = line.split_once(':').map(|(_, rest)| rest)?;
    after_colon.split_whitespace().next()
}

/// True if the line's mnemonic is a control-transfer to a named target.
/// aarch64: `bl` (call), `b` (tail-call). x86_64: `call` / `callq`.
/// Indirect/register variants (`blr` / `br` / `callq *...`) don't
/// reference named symbols so they're not matter for our assertions —
/// we still flag them via the simple mnemonic check, but the symbol
/// substring won't be present in the operands so they won't trip the
/// assertion either way.
fn is_branch_instruction(line: &str) -> bool {
    matches!(
        extract_mnemonic(line),
        Some("bl") | Some("b") | Some("call") | Some("callq")
    )
}

/// Assert that NO `bl`/`call`/`b ` instruction inside any function
/// whose demangled name contains `in_function_substr` targets a
/// symbol whose demangled name contains `forbidden_callee_substr`.
///
/// Use to assert "callee was inlined into caller" — if it had not
/// been, a branch instruction targeting the callee would remain.
pub fn assert_no_call_to_symbols_matching(
    ctx: &DisasmContext,
    in_function_substr: &str,
    forbidden_callee_substr: &str,
) {
    let bodies = ctx.bodies_of(in_function_substr);
    assert!(
        !bodies.is_empty(),
        "{}: no function matching `{}` found in disassembly — \
         symbol-naming drift? Functions present:\n{}",
        ctx.project_name,
        in_function_substr,
        ctx.functions
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut violations: Vec<String> = Vec::new();
    for body in bodies {
        for line in body {
            if is_branch_instruction(line) && line.contains(forbidden_callee_substr) {
                violations.push(line.clone());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{}: expected `{}` to NOT call `{}` (callee should be inlined), \
         but found:\n{}",
        ctx.project_name,
        in_function_substr,
        forbidden_callee_substr,
        violations.join("\n"),
    );
}

/// Assert that AT LEAST ONE `bl`/`call`/`b ` instruction inside any
/// function whose demangled name contains `in_function_substr`
/// targets a symbol whose demangled name contains
/// `required_callee_substr`.
///
/// Use to assert "callee was NOT inlined" — e.g. for
/// `#[inline(never)]` callees, no-LTO cross-crate boundaries,
/// `opt-level = 0` baselines.
pub fn assert_call_to_symbol_matching(
    ctx: &DisasmContext,
    in_function_substr: &str,
    required_callee_substr: &str,
) {
    let bodies = ctx.bodies_of(in_function_substr);
    assert!(
        !bodies.is_empty(),
        "{}: no function matching `{}` found in disassembly — \
         symbol-naming drift? Functions present:\n{}",
        ctx.project_name,
        in_function_substr,
        ctx.functions
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut found = false;
    for body in bodies {
        for line in body {
            if is_branch_instruction(line) && line.contains(required_callee_substr) {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(
        found,
        "{}: expected `{}` to call `{}` (callee should NOT be inlined), \
         but no matching branch instruction found",
        ctx.project_name,
        in_function_substr,
        required_callee_substr,
    );
}
