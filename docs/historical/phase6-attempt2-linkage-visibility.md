# Phase 6 Attempt Writeup: Wrapper Internalized by Partitioner

**Date:** 2026-04-15
**Status:** Blocked on symbol-linkage / CGU-partitioning decision in rustc.
**Scope:** Phase 6 of toylang — making `Option::unwrap` / `Result::unwrap` callable from toylang-generated LLVM IR.

---

## TL;DR

The implementation of Phase 6 (generic wrapper functions in `__lang_stubs` to redirect toylang method calls around inline stdlib methods) is architecturally correct and fully wired. Rustc's monomorphization collector successfully picks up the wrapper instance via our `ReifyFnPointer`-based dep-registration mechanism, generates a monomorphization entry for `__lang_stubs::__toylang_option_unwrap::<i32>`, and schedules it for codegen. However, **rustc's CGU partitioner assigns it `Linkage::Internal`**, making it invisible to toylang's separately-linked `.o` file. The linker fails with "undefined symbol."

Three approaches to force external linkage were considered; we paused before choosing one to get a second opinion from the tech lead.

---

## Full Context

### What Phase 6 is trying to do

Toylang compiles separately from rustc: toylang source is translated into LLVM IR by toylang's own Inkwell backend, producing a `.o` file that is injected into rustc's link step. When toylang code calls a Rust method like `opt.unwrap()`, toylang's LLVM backend emits an extern declaration and a direct call to the mangled symbol that rustc *would* emit for that instantiation.

This works fine for ordinary Rust methods (e.g. `Vec::push`). It breaks for `Option::unwrap` / `Result::unwrap` because those are `#[inline(always)]` — rustc inlines them at every call site and never emits a callable symbol. Toylang's extern declaration points at nothing, and the linker fails.

**The plan**: generate non-inline wrapper functions in the consumer-facing `__lang_stubs` module:

```rust
#[inline(never)]
pub fn __toylang_option_unwrap<T>(o: core::option::Option<T>) -> T {
    o.unwrap()
}
```

Toylang redirects its method dispatch so that `Option::unwrap<i32>` becomes `__toylang_option_unwrap::<i32>`. Rustc inlines the `o.unwrap()` body into the wrapper during codegen, but the wrapper itself gets an external symbol we can link against.

### Prior failed attempt

A previous implementer tried the same idea and failed. Their write-up is at `monomorphization-not-generated-case-orig.md`. Their mistake: the redirect lived only at the LLVM-codegen site (`llvm_gen::get_or_resolve_rust_method`), so they queried `tcx.symbol_name(wrapper_instance)` to get the extern name but never added the wrapper `Instance` to the calling toylang function's `rust_deps` list. Without that, `per_instance_mir` never emitted a `ReifyFnPointer` to the wrapper, rustc's collector never saw it, and the symbol was never emitted.

Before this attempt, we wrote up a detailed plan (`phase-6-plan.md`) identifying the correct integration point — dep registration in `callbacks_impl.rs::collect_toylang_fn_deps_inner` — and spawned two research agents to sanity-check assumptions. Agent findings backed up the plan:

- **Agent 1**: Wrapper lookup via `find_stub_fn_def_id` pattern works for generic fns. Per-instance-mir exclusion is guaranteed by two orthogonal checks (not `is_consumer_fn`, not `is_consumer_accessor`). Arg count and generic shape match. `pub use` and wrapper `pub fn` coexist cleanly.
- **Agent 2**: `ReifyFnPointer` + `CollectionMode::UsedItems` at `rustc_monomorphize/src/collector.rs:709-717` reliably adds the fn to `used_items` (authoritative), regardless of the `Unreachable` terminator. Claimed "cross-CGU reference + `-C codegen-units=16` forces external linkage." *This claim turned out to be wrong — see below.*

---

## What we implemented

### 1. `toylangc/src/oracle.rs` — wrapper table and redirect helper

```rust
/// Phase 6: table of stdlib methods that can't be called directly because
/// they are `#[inline(always)]` (no external symbol emitted) or `#[track_caller]`
/// (hidden ABI param). For each, stub_gen emits a `pub fn __toylang_*` wrapper
/// in __lang_stubs; dep registration and codegen both redirect to the wrapper.
const WRAPPERS: &[(&str, &str, &str)] = &[
    ("Option", "unwrap", "__toylang_option_unwrap"),
    ("Result", "unwrap", "__toylang_result_unwrap"),
];

pub fn wrapper_fn_name(type_name: &str, method_name: &str) -> Option<&'static str> {
    WRAPPERS.iter()
        .find(|(t, m, _)| *t == type_name && *m == method_name)
        .map(|(_, _, w)| *w)
}

pub fn all_wrappers() -> &'static [(&'static str, &'static str, &'static str)] {
    WRAPPERS
}

fn find_wrapper_fn_def_id(tcx: TyCtxt<'_>, wrapper_name: &str) -> Option<DefId> {
    for local_def_id in tcx.hir_crate_items(()).definitions() {
        let def_id = local_def_id.to_def_id();
        if tcx.def_kind(def_id) != DefKind::Fn { continue; }
        if tcx.item_name(def_id).as_str() != wrapper_name { continue; }
        if rustc_lang_facade::is_from_lang_stubs(tcx, def_id) {
            return Some(def_id);
        }
    }
    None
}

pub fn redirect_to_wrapper<'tcx>(
    tcx: TyCtxt<'tcx>,
    type_name: &str,
    method_name: &str,
    type_args: &[crate::toylang::typed_ast::ResolvedType],
) -> Option<(DefId, ty::GenericArgsRef<'tcx>)> {
    let wrapper_name = wrapper_fn_name(type_name, method_name)?;
    let wrapper_def_id = find_wrapper_fn_def_id(tcx, wrapper_name)?;
    let all_ty_args: Vec<ty::GenericArg<'tcx>> = type_args.iter()
        .map(|ta| ty::GenericArg::from(resolved_to_rustc_ty(tcx, ta)))
        .collect();
    let expected_count = tcx.generics_of(wrapper_def_id).count();
    let args = tcx.mk_args(&all_ty_args[..expected_count.min(all_ty_args.len())]);
    Some((wrapper_def_id, args))
}
```

### 2. `toylangc/src/toylang/callbacks_impl.rs` — inject redirect into dep collection

The inherent-method branch of `collect_toylang_fn_deps_inner` (which previously just did `find_inherent_method` + `tcx.mk_args`) now checks the wrapper table first:

```rust
// Inherent method call. Phase 6: redirect to wrapper if applicable so the
// wrapper Instance — not the inline stdlib method — lands in rust_deps.
// per_instance_mir then reifies a fn pointer to it, forcing codegen.
if let Some((wdef, wargs)) = crate::oracle::redirect_to_wrapper(
    tcx, &dep.type_name, &dep.method_name, &dep.type_args,
) {
    eprintln!("[phase6] redirect {}.{} → wrapper def_id={:?} args={:?}",
        dep.type_name, dep.method_name, wdef, wargs);
    deps.push((wdef, wargs));
    continue;
}

let type_def_id = crate::oracle::find_rust_type_def_id(tcx, &dep.type_name)...
// (existing logic unchanged)
```

### 3. `toylangc/src/llvm_gen.rs` — inject same redirect into LLVM codegen path

The inherent branch of `get_or_resolve_rust_method`:

```rust
} else {
    // Inherent method call. Phase 6: redirect to wrapper if applicable so
    // the extern decl and call use the wrapper symbol — matching the
    // Instance that dep-registration pushed into rust_deps.
    if let Some((wdef, wargs)) = crate::oracle::redirect_to_wrapper(
        self.tcx, type_name, method_name, type_args,
    ) {
        (wdef, wargs)
    } else {
        let type_def_id = crate::oracle::find_rust_type_def_id(self.tcx, type_name)...
        // (existing logic unchanged)
        (method_def_id, args)
    }
};
```

Critical: both sites call the same `oracle::redirect_to_wrapper` so `tcx.symbol_name` produces identical output for the dep Instance and the extern-declaration Instance.

### 4. `toylangc/src/stub_gen.rs` — emit the wrappers

```rust
let phase6_wrappers: Vec<syn::Item> = vec![
    parse_quote! {
        #[inline(never)]
        pub fn __toylang_option_unwrap<T>(o: core::option::Option<T>) -> T {
            o.unwrap()
        }
    },
    parse_quote! {
        #[inline(never)]
        pub fn __toylang_result_unwrap<T, E: core::fmt::Debug>(r: core::result::Result<T, E>) -> T {
            r.unwrap()
        }
    },
];
items.extend(phase6_wrappers);
```

Debug dump of a generated `__lang_stubs.rs` for the test below (via a `TOYLANG_DUMP_STUBS=1` env-gate):

```rust
pub use std::option::Option;
extern "C" {
    pub fn __toylang_impl_main() -> ();
}
pub fn __toylang_main() -> () {
    unreachable!()
}
#[inline(never)]
pub fn __toylang_option_unwrap<T>(o: core::option::Option<T>) -> T {
    o.unwrap()
}
#[inline(never)]
pub fn __toylang_result_unwrap<T, E: core::fmt::Debug>(
    r: core::result::Result<T, E>,
) -> T {
    r.unwrap()
}
```

### 5. Integration test

```rust
#[test]
fn test_option_unwrap_basic() {
    // Option::unwrap is #[inline(always)] — direct call would produce no
    // external symbol. Phase 6 redirects to __toylang_option_unwrap wrapper
    // in __lang_stubs, which rustc compiles normally.
    let output = run_toylang_test(
        r#"
use std::option::Option

fn make_some_i32(x: i32) -> Option<i32>
fn println_i32(x: i32)

fn main() {
    let o = make_some_i32(42i32);
    let v = o.unwrap();
    println_i32(v)
}
        "#,
        r#"
mod __lang_stubs;
use __lang_stubs::*;
#[no_mangle]
pub fn make_some_i32(x: i32) -> Option<i32> { Some(x) }
#[no_mangle]
pub fn println_i32(x: i32) { println!("{}", x); }
fn main() { __toylang_main(); }
        "#,
    );
    assert!(output.contains("42"));
}
```

---

## What happened when we ran it

### First run (no diagnostics)

```
cargo +rustc-fork test -p toylangc --test integration_tests test_option_unwrap_basic -- --nocapture
```

Output (linker error):

```
Undefined symbols for architecture arm64:
  "test::__lang_stubs::__toylang_option_unwrap::hf0150b200c563b5e", referenced from:
      ___toylang_internal_main in toylang_output_82131.o
ld: symbol(s) not found for architecture arm64
```

The demangled symbol shows the path `test::__lang_stubs::__toylang_option_unwrap` with legacy-mangler hash suffix. Exactly the failure signature of the previous attempt.

### Diagnostics added

To isolate the stage of failure, we added three diagnostic hooks:

**1.** `stub_gen.rs`: dump generated stubs under `TOYLANG_DUMP_STUBS`:
```rust
if std::env::var("TOYLANG_DUMP_STUBS").is_ok() {
    eprintln!("=== __lang_stubs.rs ===\n{}\n=== end ===", out);
}
```

**2.** `callbacks_impl.rs`: log when redirect fires:
```rust
eprintln!("[phase6] redirect {}.{} → wrapper def_id={:?} args={:?}",
    dep.type_name, dep.method_name, wdef, wargs);
```

**3.** `rustc-lang-facade/src/queries/per_instance.rs`: log each dep being reified in `build_dependency_body`:
```rust
eprintln!("[per_instance_mir dep] {:?} args={:?} fn_like={} from_lang_stubs={}",
    dep_def_id, dep_args,
    tcx.def_kind(dep_def_id).is_fn_like(),
    crate::is_from_lang_stubs(tcx, dep_def_id));
```

**4.** `toylangc/src/main.rs`: forward `-Zprint-mono-items=lazy` under `TOYLANG_PRINT_MONO`:
```rust
if std::env::var("TOYLANG_PRINT_MONO").is_ok() {
    args.push("-Zprint-mono-items=lazy".to_string());
}
```

### Diagnostic output

```
=== __lang_stubs.rs ===
...
#[inline(never)]
pub fn __toylang_option_unwrap<T>(o: core::option::Option<T>) -> T {
    o.unwrap()
}
...
=== end ===

[phase6] redirect Option.unwrap → wrapper def_id=DefId(0:8 ~ test[0374]::__lang_stubs::__toylang_option_unwrap) args=[i32]

[per_instance_mir dep] DefId(0:14 ~ test[0374]::make_some_i32) args=[] fn_like=true from_lang_stubs=false
[per_instance_mir dep] DefId(0:15 ~ test[0374]::println_i32) args=[] fn_like=true from_lang_stubs=false
[per_instance_mir dep] DefId(0:8 ~ test[0374]::__lang_stubs::__toylang_option_unwrap) args=[i32] fn_like=true from_lang_stubs=true
```

All three signals fire correctly:
- Stubs file contains the `#[inline(never)]` generic wrapper.
- Oracle redirect produces the right `(wrapper_def_id, [i32])`.
- `per_instance_mir` receives the wrapper as one of `__toylang_main`'s three rust deps, with `fn_like=true` — so it builds a `ReifyFnPointer` cast to `Instance { def_id: wrapper, args: [i32] }` in `__toylang_main`'s synthesized MIR body.

The `-Zprint-mono-items=lazy` output was the smoking gun:

```
MONO_ITEM fn __lang_stubs::__toylang_main               @@ test.3743c738498b5a4-cgu.3[External]
MONO_ITEM fn __lang_stubs::__toylang_option_unwrap::<i32> @@ test.3743c738498b5a4-cgu.3[Internal]
MONO_ITEM fn std::option::Option::<i32>::unwrap        @@ test.3743c738498b5a4-cgu.3[Internal]
MONO_ITEM fn make_some_i32                              @@ test.3743c738498b5a4-cgu.2[External]
MONO_ITEM fn println_i32                                @@ test.3743c738498b5a4-cgu.2[External]
```

- Collector found the wrapper. ✓
- Partitioner placed it in a CGU. ✓
- But marked it `[Internal]`. ✗ That's what's blocking us.
- Both `__toylang_main` and the wrapper landed in the same CGU (cgu.3).
- `Option::unwrap::<i32>` also gets mono'd (because rustc inlined it into the wrapper's body) and is also Internal. That's fine — it's called directly from the wrapper, intra-CGU.

So rustc successfully monomorphizes the wrapper. The architectural fix works. The failure is purely in linkage assignment.

---

## Root cause in rustc's partitioner

Traced through `/Users/verdagon/rust/compiler/rustc_monomorphize/src/partitioning.rs`.

### The internalization decision

`internalize_symbols` (line 519 of partitioning.rs) marks an item `Linkage::Internal` if it's an "internalization candidate" AND none of its users live in a different CGU:

```rust
for (item, data) in cgu.items_mut() {
    if !internalization_candidates.contains(item) { continue; }
    if !single_codegen_unit {
        if cx.usage_map.get_user_items(*item).iter()
            .filter_map(|user_item| mono_item_placements.get(user_item))
            .any(|placement| *placement != home_cgu)
        {
            continue;  // has a cross-CGU user — skip internalization
        }
    }
    data.linkage = Linkage::Internal;   // ← our wrapper lands here
    data.visibility = Visibility::Default;
}
```

The wrapper's only user is `__toylang_main` (via the `ReifyFnPointer` in our synthesized MIR body). Both are in cgu.3. So `get_user_items` returns only in-CGU users → wrapper is internalized.

### Why the wrapper is an internalization candidate

`mono_item_linkage_and_visibility` (line 734) and `mono_item_visibility` (line 769):

```rust
let is_generic = instance.args.non_erasable_generics().next().is_some();  // true for us

// (local def_id path)
if is_generic {
    if always_export_generics
        || (can_export_generics
            && tcx.codegen_fn_attrs(def_id).inline == InlineAttr::Never)
    {
        if tcx.is_unreachable_local_definition(def_id) {
            Visibility::Hidden
        } else {
            *can_be_internalized = false;   // ← the branch we need
            default_visibility(tcx, def_id.to_def_id(), true)
        }
    } else {
        Visibility::Hidden   // ← where we actually end up
    }
}
```

Our wrapper is `#[inline(never)]`, so the `inline == InlineAttr::Never` check passes. But we also need `can_export_generics`, which comes from:

```rust
// rustc_middle/src/ty/context.rs:1938
pub fn local_crate_exports_generics(self) -> bool {
    self.crate_types().iter().any(|crate_type| match crate_type {
        CrateType::Executable | CrateType::Staticlib
        | CrateType::ProcMacro | CrateType::Cdylib => false,
        CrateType::Dylib | CrateType::Rlib => true,
    })
}
```

The test compiles as `CrateType::Executable` → `local_crate_exports_generics()` returns `false` → `can_export_generics = false` → we fall into the `else` branch returning `Visibility::Hidden` with `can_be_internalized = true` (the default). Then in the caller (line 254):

```rust
if visibility == Visibility::Hidden && can_be_internalized {
    internalization_candidates.insert(mono_item);
}
```

The wrapper becomes a candidate. Internalization runs. Linkage → Internal.

### The fast-path that would save us

`mono_item_linkage_and_visibility` first checks for explicit linkage:

```rust
fn mono_item_linkage_and_visibility<'tcx>(...) -> (Linkage, Visibility) {
    if let Some(explicit_linkage) = mono_item.explicit_linkage(tcx) {
        return (explicit_linkage, Visibility::Default);   // bypasses all the above
    }
    ...
}
```

where `explicit_linkage` returns `tcx.codegen_fn_attrs(def_id).linkage` (mono.rs:154–155), which is populated by the `#[linkage = "external"]` attribute. That would completely avoid the internalization path.

### Agent 2's "cross-CGU reference" claim was wrong

Earlier we cited Agent 2's finding: "cross-CGU reference + `-C codegen-units=16` forces external linkage." In practice, the partitioner placed both the caller and the wrapper in the *same* CGU, so the cross-CGU reference the agent hypothesized doesn't materialize. `-C codegen-units=16` is still load-bearing (it allows `make_some_i32`, `println_i32`, etc. to be in *different* CGUs, which is why they're External), but the wrapper and its sole caller stay together. The previous plan's gotcha 1a needs to be rewritten — external linkage is NOT a side effect of `codegen-units` alone.

---

## Options we considered

### Option A — `#[linkage = "external"]` on the wrapper

```rust
#[inline(never)]
#[linkage = "external"]
pub fn __toylang_option_unwrap<T>(...) -> T { o.unwrap() }
```

- Bypasses the visibility/internalization path entirely via `mono_item.explicit_linkage`.
- **Requires `#![feature(linkage)]`** at the **crate root** — not inside `__lang_stubs`, since module-level feature attrs don't exist. The test crate's `test.rs` would need `#![feature(linkage)]` at the top.
- We tried this briefly and reverted because adding a feature gate to user-written Rust test files is brittle and user-visible. Toylang compiles arbitrary user Rust; requiring every consumer to add `#![feature(linkage)]` to survive Phase 6 is not acceptable.
- A possible mitigation: have toylang's driver inject `--cfg feature_linkage` or manipulate the crate attribute list programmatically. But this gets into rustc-internals territory.

### Option B — Patch the rustc fork

We already maintain a forked rustc at `/Users/verdagon/rust`. A 2–4 line patch in `partitioning.rs::mono_item_visibility` that forces `can_be_internalized = false` for `__lang_stubs` items would fix this cleanly. Sketch:

```rust
// rustc_monomorphize/src/partitioning.rs, in mono_item_visibility
if is_generic {
    // Phase 6 (toylang): items in __lang_stubs may be referenced only from
    // externally-linked consumer .o files, which the partitioner cannot see
    // from its in-CGU usage map. Force external linkage for them.
    if is_from_lang_stubs(tcx, def_id.to_def_id()) {
        *can_be_internalized = false;
        return default_visibility(tcx, def_id.to_def_id(), true);
    }
    // ... existing generic path
}
```

`is_from_lang_stubs` is currently defined in the `rustc-lang-facade` crate (consumer-side). To call it from inside rustc, we'd either (a) inline the check (`tcx.def_path(def_id)` contains `__lang_stubs`), or (b) add a new rustc-internal query hook the facade can set at startup. Option (a) is 1 line and pragmatic; option (b) is cleaner but more scope.

- Pros: Pure compiler-side change. No user-visible requirement. Scales to any future wrapper without per-call-site work.
- Cons: Touches the fork. Makes the partitioner aware of a consumer-specific concept (`__lang_stubs`), which conflates layering — though the *entire* fork already does this at multiple hooks (`per_instance_mir`, `symbol_name`, codegen skip), so it's not a new sin.

### Option C — Per-instantiation non-generic shim layer

For every call site in toylang that needs a wrapper, emit a non-generic `#[no_mangle]` wrapper calling into the generic wrapper:

```rust
// generic, stays internal — that's fine because it's only called from:
#[inline(never)]
pub fn __toylang_option_unwrap<T>(o: Option<T>) -> T { o.unwrap() }

// non-generic, #[no_mangle] forces external linkage
#[no_mangle]
pub fn __toylang_option_unwrap_i32(o: Option<i32>) -> i32 {
    __toylang_option_unwrap::<i32>(o)
}
```

Non-generic `pub` functions with `#[no_mangle]` always get external linkage, and the outer call forces monomorphization of the generic wrapper (which can then stay Internal — invisible to us but fine, because rustc-internal code calls it).

- Pros: Uses only stable, vanilla Rust. No fork patch, no feature gate.
- Cons: stub_gen currently runs *before* we know which `T`s toylang will use. The previous implementer called this a "workaround," but it is in fact the strategy Rust projects normally use for C FFI. The coordination issue is real but solvable: we could (a) pre-generate shims for every toylang-visible type eagerly (simple, wastes some symbols for unused types), or (b) do a toylang pre-pass that collects the needed instantiations before generating stubs.
- This was "Attempt 3" of the previous failed attempt, which they described as working but rejected for aesthetic reasons.

### Option D — Force a cross-CGU split

In theory, if we could convince the partitioner to place the wrapper in a *different* CGU from `__toylang_main`, the cross-CGU usage check would skip internalization.

- No clean knob for this from Rust source. `#[cold]` doesn't affect partitioning. Inserting artificial "heat" boundaries would be a hack.
- Dismissed as non-viable.

### Option E — Do nothing at the wrapper, use a `#[used]` static pointing at each monomorphization

```rust
#[used]
static __TOYLANG_WRAPPER_I32: fn(Option<i32>) -> i32 = __toylang_option_unwrap::<i32>;
```

Has the same "know T upfront" problem as option C. Plus `#[used]` prevents DCE but doesn't itself promote linkage — we'd end up with the static exported but the fn pointer it targets still internal. Also, the previous attempt tried a similar synthetic static and it caused a rustc ICE (see `monomorphization-not-generated-case-orig.md` attempt 2).

---

## Files currently modified (still present in working tree)

- `toylangc/src/oracle.rs` — wrapper table, `redirect_to_wrapper`, `find_wrapper_fn_def_id`. Keep.
- `toylangc/src/toylang/callbacks_impl.rs` — redirect injected in inherent method branch of `collect_toylang_fn_deps_inner`. Has a debug `eprintln` we'd remove before merging. Keep the redirect.
- `toylangc/src/llvm_gen.rs` — redirect injected in inherent method branch of `get_or_resolve_rust_method`. Keep.
- `toylangc/src/stub_gen.rs` — emits `__toylang_option_unwrap` and `__toylang_result_unwrap`. Has the `TOYLANG_DUMP_STUBS` diagnostic. Keep wrappers; may remove dump gate.
- `toylangc/src/main.rs` — `TOYLANG_PRINT_MONO` → `-Zprint-mono-items=lazy` gate. Diagnostic only; remove or keep as a dev tool.
- `rustc-lang-facade/src/queries/per_instance.rs` — `[per_instance_mir dep]` eprintln. Diagnostic only.
- `toylangc/tests/integration_tests.rs` — `test_option_unwrap_basic`. Keep.

Nothing has been committed. All changes live in the working tree.

---

## Related plan documents

- `phase-6-plan.md` — the implementation plan. Gotcha 1a ("external linkage comes from cross-CGU reference + `-C codegen-units=16`") is incorrect as written. The external linkage story needs to be rewritten based on whichever option we pick.
- `monomorphization-not-generated-case-orig.md` — the previous attempt's write-up. Their failure mode was earlier in the pipeline (wrapper never mono'd at all). Our failure mode is *later* (mono'd but internalized).
- `docs/historical/phase6-inline-functions-investigation.md` — the investigation doc the plan cites.

---

## Questions for the tech lead

1. **Which option?** Our instinct is B (rustc fork patch) because it's smallest, but it conflates layers. C (per-instantiation shims) is cleaner from rustc's POV but needs coordination work in stub_gen.

2. **If option B, where does the `is_from_lang_stubs` check belong?** Inline the `def_path`-string match inside `partitioning.rs`, or add a query hook the facade sets? The former keeps rustc standalone-buildable; the latter matches existing facade patterns.

3. **For option C, is there prior art for deferring stub generation until after the toylang pre-pass?** The current architecture has `generate_stubs()` fire once at driver start (driver.rs:44). Would we want a second pass, or have stub_gen eagerly emit shims for all `{wrapper × known_toylang_type}` combinations?

4. **Is there an Option F we haven't considered?** Perhaps something involving the facade's existing `symbol_name` override (`rustc-lang-facade/src/queries/symbol_name.rs`), or a per-instance codegen override for specifically forcing external visibility on wrappers.

5. **Is internal linkage *actually* a problem at the object-file level, or only in rustc's LLVM emission?** Could a post-processing step on rustc's `.o` files re-expose those symbols before the link step? Unlikely to be cleaner than fixing it at source, but noted.