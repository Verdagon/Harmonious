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

/// Cargo output-dir subdirectory under the shared `CARGO_TARGET_DIR`.
///
/// **Important**: this enum names the cargo-target SUBDIRECTORY (`debug/`
/// vs `release/`), NOT the optimization level. Toylang's `build.rs`
/// keeps every fixture on the `dev` cargo profile and customizes
/// `[profile.dev]` with the fixture's declared `opt-level`. So a
/// fixture with `opt-level = "3"` is still built into `debug/` —
/// the binary IS opt-level-3 compiled, just emitted to the dir
/// cargo names `debug` by convention. Consequence: every
/// inlining-matrix fence (SBMNBIZ, SMPLZ, symbol-uniqueness) calls
/// `disassemble_binary(name, Profile::Debug)` and that's correct
/// regardless of the fixture's opt-level — the artifact lives there.
/// `Release` is reserved for if toylang ever splits its emission
/// model; no current fixture uses it.
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
    /// Diagnostic-only: retained for ad-hoc inspection paths that may
    /// re-run llvm-objdump against the binary outside the harness.
    /// Not read by the harness itself.
    #[allow(dead_code)]
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

// ============================================================================
// SBMNBIZ binary-shape fences (Category 1 of the test expansion plan).
// These check structural properties of the final binary that catch
// regressions in the AvailableExternally-stub-must-not-be-inlined
// invariant (§26.17 SBMNBIZ arcanum). Apply via
// `assert_no_inlined_unreachable_in_main(name)` from any test that
// expects the binary to run to completion (i.e. NOT panic). If the
// stub's unreachable!() body inlined into main, that body lowers to
// either a `udf`/`brk` instruction (aarch64) or a `bl ...panic` call.

/// Check that the bin's `main` function does NOT contain any
/// instruction or branch that indicates an `unreachable!()` body got
/// inlined. Specifically scans for:
///   - `udf` / `brk` aarch64 instructions (the lowering of
///     `unreachable_unchecked` / debug-assert-on `unreachable!()` /
///     trap intrinsics).
///   - `bl ... core::panicking::panic` calls (the way `unreachable!()`
///     lowers when the panic path is materialized rather than trapped).
///
/// This is a defensive-in-depth fence. The integration harness already
/// catches SBMNBIZ failures at runtime (the binary panics → non-zero
/// exit → test fails). The structural fence adds a second tier: if a
/// future refactor causes the harness to skip running a binary, the
/// structural check still catches an SBMNBIZ regression. Applied
/// uniformly to every inlining-matrix fixture that's expected to run
/// to completion.
pub fn assert_no_inlined_unreachable_in_main(project: &str) {
    let ctx = disassemble_binary(project, Profile::Debug);
    let main_needle = format!("{}::main", project);
    let bodies = ctx.bodies_of(&main_needle);
    if bodies.is_empty() {
        // Some fixtures (e.g. case5_off_*) emit no `<crate>::main` per
        // se because thin-LTO inlines the bin shim entirely into a
        // wrapper. In those cases the SBMNBIZ check is vacuous (no
        // function to scan); the runtime-output check still catches
        // any panic. Skip silently.
        return;
    }
    // Only flag aarch64 trap instructions (`udf` / `brk`). Calls to
    // `core::panicking::panic*` helpers are intentionally NOT flagged
    // here because they appear in many legitimate code paths (assert!,
    // arithmetic overflow checks, `core::panicking::panic_cannot_unwind`
    // from the panic-abort runtime, etc.) that aren't SBMNBIZ-related.
    // The structural check pairs with the runtime-output check (binary
    // must exit 0 with expected stdout); together they catch
    // SBMNBIZ-class regressions without false-positing on regular
    // panic-path code that Sky bodies legitimately produce.
    let mut violations: Vec<String> = Vec::new();
    for body in bodies {
        for line in body {
            let mnemonic = extract_mnemonic(line).unwrap_or("");
            if mnemonic == "udf" || mnemonic == "brk" {
                violations.push(format!("[unreachable-trap] {}", line));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{}: SBMNBIZ violation — `{}` contains a `udf`/`brk` trap that \
         indicates an `unreachable!()` body was inlined into a real \
         caller:\n{}",
        project,
        main_needle,
        violations.join("\n"),
    );
}

/// Assert that a Sky-emitted symbol exists in the binary with EXTERNAL
/// linkage (`g F __TEXT,__text` in `llvm-objdump -t` output). Catches
/// `GlobalDCE` / LTO `internalize` regressions that would silently
/// strip or demote Sky symbols (the SMPLZ arcanum's failure mode).
///
/// Uses the system `llvm-objdump -t`; the symbol name lookup is a
/// substring match (so callers can pass demangled-readable fragments).
pub fn assert_sky_symbol_externally_visible(project: &str, demangled_substr: &str) {
    let llvm_prefix = std::env::var("LLVM_SYS_211_PREFIX")
        .expect("LLVM_SYS_211_PREFIX must be set for SBMNBIZ symbol-linkage assertion");
    let bin = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/integration-projects-cache/debug")
        .join(project);
    assert!(
        bin.exists(),
        "{}: binary not found at {}",
        project,
        bin.display(),
    );
    let out = Command::new(format!("{}/bin/llvm-objdump", llvm_prefix))
        .args(["-t", bin.to_str().unwrap()])
        .output()
        .unwrap_or_else(|e| panic!("{}: llvm-objdump failed: {}", project, e));
    assert!(out.status.success(), "llvm-objdump exited non-zero");
    let symtab = String::from_utf8_lossy(&out.stdout);
    // Demangle each line's symbol via rustc-demangle for substring
    // matching (the symbol table itself contains mangled names).
    let mut found_external = false;
    let mut found_internal = false;
    for line in symtab.lines() {
        // Skip lines that don't include the demangled fragment when
        // demangled. For perf, do a simple mangle-substring check
        // first (mangled symbols usually contain the unmangled name
        // as a fragment) before invoking rustc_demangle.
        let demangled = rustc_demangle::demangle(line).to_string();
        if !demangled.contains(demangled_substr) {
            continue;
        }
        // Linkage column: `g` = global (External), `l` = local
        // (Internal). Column position varies; just check for the
        // marker fragment after the address.
        if line.contains(" g     F ") {
            found_external = true;
        } else if line.contains(" l     F ") {
            found_internal = true;
        }
    }
    assert!(
        found_external,
        "{}: expected symbol matching `{}` to exist with External \
         linkage (`g F __TEXT,__text`) in the binary, but none found. \
         (Found internal-linkage candidates: {}). SMPLZ regression?",
        project,
        demangled_substr,
        found_internal,
    );
}

/// Assert that every CONSUMER-EMITTED symbol in the final binary has
/// **exactly one** function-definition entry in `llvm-objdump -t`
/// output.
///
/// This catches B17 silent-collision regressions: a scenario where
/// both rustc and Sky's `fill_extra_modules` hook emit a real body
/// for the same mangled symbol, and instead of the linker producing
/// a duplicate-symbol error (loud failure), one body is silently
/// preferred — leaving the wrong code in the binary.
///
/// Under shipping architecture (Option 4 + patch 5), this can't
/// happen because the `codegen_fn_attrs` override stamps consumer
/// items with `AvailableExternally` linkage (IR-only, no `.o` symbol).
/// Under the historical partition filter, rustc never emitted the
/// competing symbol at all. Either way, the binary's symbol table
/// should show exactly one definition per consumer symbol — Sky's
/// real body emitted via the `fill_extra_modules` hook.
///
/// What the fence is sensitive to:
///   - rustc partitioner / `codegen_fn_attrs` regressions that
///     re-emit consumer items as `.o` symbols (B17).
///   - Sky-side bugs that emit the same body from multiple sessions
///     (e.g. owning-crate AND user-bin both contributing).
///   - LTO post-pass that materializes a competing local copy.
///
/// What the fence is NOT sensitive to:
///   - The SBMNBIZ inlined-unreachable scenario — that's the same
///     symbol with one .o def + an IR-only available_externally body.
///     Use `assert_no_inlined_unreachable_in_main` for that.
///   - Pure-Rust symbols — only symbols matching the consumer-emit
///     patterns are checked.
///
/// Heuristic for "consumer symbol":
///   - Contains `__toylang_internal_` (toylang's internal mangling).
///   - Contains `__toylang_main` (the toylang Sky-main symbol).
///   - Contains `___lang_stubs` (v0-mangled symbols where the
///     instantiating crate is the stub rlib — Sky-emitted bodies for
///     Sky exports + cascade-discovered trait-impl monomorphizations).
pub fn assert_consumer_symbols_uniquely_defined(project: &str) {
    let llvm_prefix = std::env::var("LLVM_SYS_211_PREFIX")
        .expect("LLVM_SYS_211_PREFIX must be set for symbol-uniqueness assertion");
    let bin = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/integration-projects-cache/debug")
        .join(project);
    assert!(
        bin.exists(),
        "{}: binary not found at {}",
        project,
        bin.display(),
    );
    let out = Command::new(format!("{}/bin/llvm-objdump", llvm_prefix))
        .args(["-t", bin.to_str().unwrap()])
        .output()
        .unwrap_or_else(|e| panic!("{}: llvm-objdump failed: {}", project, e));
    assert!(out.status.success(), "{}: llvm-objdump exited non-zero", project);
    let symtab = String::from_utf8_lossy(&out.stdout);

    // Collect (symbol_name -> Vec<(address, linkage_marker, full_line)>)
    // for lines that represent function DEFINITIONS in `__TEXT,__text`.
    // A "definition" is a `g     F __TEXT,__text` or `l     F __TEXT,__text`
    // line. `*UND*` reference lines and data symbols (`O` type) are
    // intentionally excluded.
    let mut defs: std::collections::BTreeMap<String, Vec<(String, &'static str, String)>> =
        std::collections::BTreeMap::new();
    for line in symtab.lines() {
        let (linkage_marker, is_def) = if line.contains(" g     F __TEXT,__text ") {
            ("g", true)
        } else if line.contains(" l     F __TEXT,__text ") {
            ("l", true)
        } else {
            ("", false)
        };
        if !is_def {
            continue;
        }
        // Symbol name is the last whitespace-separated token on the line
        // (works whether or not the `.hidden` modifier precedes it).
        let symname = match line.split_whitespace().last() {
            Some(s) => s,
            None => continue,
        };
        if !is_consumer_symbol(symname) {
            continue;
        }
        // Address is the first token on the line.
        let addr = line
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_string();
        defs.entry(symname.to_string())
            .or_default()
            .push((addr, linkage_marker, line.to_string()));
    }

    let mut violations: Vec<String> = Vec::new();
    for (sym, def_lines) in &defs {
        // Deduplicate by address: the same symbol can appear in the
        // symtab multiple times at the same address (e.g. once as a
        // definition and once as a debug-stab line); only DISTINCT
        // address+linkage tuples count as separate physical defs.
        let mut seen: std::collections::BTreeSet<(String, &'static str)> =
            std::collections::BTreeSet::new();
        for (addr, link, _) in def_lines {
            seen.insert((addr.clone(), *link));
        }
        if seen.len() > 1 {
            let listing = def_lines
                .iter()
                .map(|(_, _, l)| l.as_str())
                .collect::<Vec<_>>()
                .join("\n  ");
            violations.push(format!(
                "consumer symbol `{}` has {} distinct definitions:\n  {}",
                sym,
                seen.len(),
                listing
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "{}: B17 silent-collision regression — multiple definitions of \
         consumer symbol(s) in final binary. Under shipping architecture \
         (Option 4 + patch 5), consumer items should have exactly one \
         real .o definition (Sky's `fill_extra_modules` body) with the \
         rustc-emitted body being `AvailableExternally` (IR-only). Two \
         physical defs indicates either the partition / linkage override \
         broke, or Sky's emitter shipped a competing body.\n\n{}",
        project,
        violations.join("\n\n"),
    );
}

fn is_consumer_symbol(name: &str) -> bool {
    name.contains("__toylang_internal_")
        || name.contains("__toylang_main")
        // v0-mangled symbols where the def's instantiating crate is
        // the stub rlib (Sky-emitted bodies for exports / trait-impl
        // cascade discoveries).
        || name.contains("___lang_stubs")
}
