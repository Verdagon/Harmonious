# rustc Extension Points — A Step-by-Step Guide

Scoped to the rustc hooks this project actually touches, in roughly the order they fire. Accurate as of our current pin (`nightly-2026-01-20`).

## Step 0: The gate — `#![feature(rustc_private)]`

All of rustc's internal crates (`rustc_driver`, `rustc_interface`, `rustc_middle`, `rustc_codegen_ssa`, `rustc_codegen_llvm`, `rustc_monomorphize`, `rustc_hashes`, ...) are nightly-only and gated by this feature. A crate that depends on them declares it at the crate root. There's no SemVer; APIs drift across nightlies. It's the "all bets off" escape hatch.

You also need to link against the compiler's shared libraries at runtime. The idiomatic path is `$(rustc --print sysroot)/lib` — that's where `librustc_driver-*.dylib/.so` lives. Two ways to satisfy it:

- **Env var** — set `DYLD_LIBRARY_PATH` (macOS) or `LD_LIBRARY_PATH` (Linux). Fine for shells and harnesses.
- **rpath at link time** — `cargo:rustc-link-arg=-Wl,-rpath,$(rustc --print sysroot)/lib` in `build.rs`. More robust for distributed binaries.

## Step 1: The driver entry point — `rustc_driver::run_compiler`

```rust
rustc_driver::run_compiler(rustc_args, &mut my_callbacks);
```

`rustc_args` is a `&[String]` in rustc argv shape (skip argv[0]; the rest is whatever `rustc` CLI would accept). `my_callbacks` is any type implementing `rustc_driver::Callbacks`. This was a builder called `RunCompiler::new(...).run()` in earlier nightlies; PR #135880 replaced it with the free function we use now.

Small siblings:

- `rustc_driver::install_ice_hook(report_url, |_| {})` — panic hook that prints a "please report at <url>" on ICE.
- `rustc_driver::catch_with_exit_code(|| { ... })` — maps rustc's panic/early-exit patterns into a clean process exit code.

`rustc_driver` itself is an almost-empty re-export shim; all the code lives in `rustc_driver_impl` (that's what you see in stack traces). The split exists so `rustc_driver_impl` can compile in parallel with the rest of the compiler. As a consumer you depend on `rustc_driver`.

The canonical reference drivers, if you need to see how people actually use this trait in anger: `clippy_driver` (in `src/tools/clippy/` of the rust-lang/rust tree) overrides lints via `Callbacks::config`; `miri` (in `src/tools/miri/`) overrides MIR and the codegen step; the dev-guide maintains a minimal example at `rustc-dev-guide/examples/rustc-driver-example.rs`.

## Step 2: The lifecycle hooks — `rustc_driver::Callbacks`

```rust
trait Callbacks {
    fn config(&mut self, config: &mut Config) {}
    fn after_crate_root_parsing(&mut self, compiler: &Compiler, krate: &mut Crate) -> Compilation { ... }
    fn after_expansion<'tcx>(&mut self, compiler: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation { ... }
    fn after_analysis<'tcx>(&mut self, compiler: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation { ... }
}
```

All methods have default implementations. Two matter for most extension work:

- **`config`** fires very early, before parsing. You mutate the passed-in `Config` to install query overrides, swap the codegen backend, etc. This is your only chance to hook things in before rustc starts real work.
- **`after_analysis`** fires after type-checking / borrow-checking, before monomorphization. You get a `TyCtxt<'tcx>` and can walk the HIR, run validation, emit diagnostics. Return `Compilation::Continue` to proceed or `Compilation::Stop` to halt.

`after_crate_root_parsing` and `after_expansion` are newer additions to the trait for intercepting earlier phases. Most consumers ignore them and rely on the defaults.

## Step 3: The config struct — `rustc_interface::Config`

Two fields are relevant:

```rust
config.override_queries    = Some(my_override_fn);
config.make_codegen_backend = Some(Box::new(|opts, target| MyBackend::new()));
```

Note the signature of `override_queries`: it's `Option<fn(&Session, &mut Providers)>` — a bare function pointer, not `Box<dyn Fn…>`. Your override must be non-capturing. Per-invocation state has to live in a `static` / `OnceLock` / thread-local that the `fn` reads; you cannot close over a `&mut` of your callbacks struct.

`make_codegen_backend` is a boxed closure that receives the current `SessionOptions` and `TargetTriple`, returning `Box<dyn CodegenBackend>`. The target param lets backends self-configure per target; most backends ignore both args.

## Step 4: The query provider table — `rustc_middle::util::Providers`

Rustc's compilation is a graph of **queries** — demand-driven, memoized computations each keyed by some input and returning some output. `layout_of`, `symbol_name`, `optimized_mir`, `fn_abi_of_instance`, and hundreds more. They are the backbone of incremental compilation.

The top-level `Providers` struct groups three sub-tables: `queries` (the main fn-pointer table, one field per query), `extern_queries` (separate providers for queries with the `separate_provide_extern` modifier — i.e., queries that must answer differently for cross-crate items), and `hooks` (a handful of non-query extension points). The override function your code installs:

```rust
fn my_override(_sess: &Session, providers: &mut Providers) {
    // Save the default if you want to delegate:
    DEFAULT_OPTIMIZED_MIR.set(providers.queries.optimized_mir).ok();
    providers.queries.optimized_mir = my_provider_fn;
}
```

The idiomatic pattern: stash the default in a `OnceLock`, have your replacement either handle the input itself or fall through for inputs you don't care about.

**How keys and keying work.** Each query's key type is whatever its `Key` impl says it is: `LocalDefId` for queries that only apply to the crate being compiled (cross-crate access is a type error, not an `expect_local()` panic), `Instance<'tcx>` for queries keyed on monomorphized generic args, `PseudoCanonicalInput<'tcx, T>` for queries like `layout_of` that also need a `TypingEnv`. Every key must be stable-hashable — its fingerprint becomes the dep-graph node ID used for incremental reuse.

**What overriding a query means for incremental compilation.** Each memoized call becomes a dep-graph node whose identity is the key's fingerprint; the result hash is compared against the previous session's to decide green vs red. If you override, your provider is what gets fingerprinted — non-deterministic output (reading a clock, iterating a `HashMap`) silently defeats incremental reuse, and side effects you make outside `tcx` won't be replayed on green reuse. Write providers as pure functions of their inputs.

The six queries this project overrides — each with distinct key, return type, and firing semantics.

### 4a. `optimized_mir`

| | |
|---|---|
| **Key** | `LocalDefId` |
| **Returns** | `&'tcx Body<'tcx>` — the optimized MIR for the function |
| **Fires** | during monomorphization, once per function whose MIR is demanded |

Return a hand-built MIR body and rustc's mono collector walks whatever you return. Call terminators queue callees for monomorphization. `Rvalue::Cast(CastKind::PointerCoercion(ReifyFnPointer(Safety::Safe), _))` queues the target Instance for codegen even if it's never called — this is the mechanism for driving emission of a generic item you reference by name but don't have a call site for. (Note the `Safety` arg on `ReifyFnPointer`; older nightlies lacked it.)

Constructing valid MIR is fiddly:

- `Statement` and `BasicBlockData` are non-exhaustive structs — use the constructor functions (`Statement::new(source_info, kind)`, `BasicBlockData::new_stmts(stmts, terminator, is_cleanup)`), not struct literals.
- `set_required_consts` and `set_mentioned_items` must be called on synthetic bodies (normal `mir_promoted` doesn't run for them; the mono collector panics if these aren't set).
- Every `BasicBlockData` needs a terminator. `TerminatorKind::Unreachable` is valid for bodies that are never executed.
- `TypingEnv::fully_monomorphized()` is a typing-*mode* flag (`PostAnalysis` typing-mode + empty bounds), not an input assertion — bodies containing `ty::TyKind::Param` placeholders flow through cleanly; rustc's collector substitutes them per caller when walking the body.

Between MIR passes rustc runs `rustc_mir_transform::validate::Validator`, which enforces invariants appropriate to the current `MirPhase` — well-typed assignments, terminator/edge-kind consistency, cast input/output shapes, no critical call edges. Hand-built MIR usually fails here first. Run tests with `-Zvalidate-mir` set to catch violations the compile step misses.

### 4b. `symbol_name`

| | |
|---|---|
| **Key** | `Instance<'tcx>` — a `(DefId, GenericArgsRef<'tcx>)` bundle with erased late-bound regions |
| **Returns** | `ty::SymbolName<'tcx>` |
| **Fires** | when rustc needs the linker-visible name for a specific monomorphization |

Return any string via `SymbolName::new(tcx, &s)`. Useful for redirecting mangled names to your own scheme.

Important subtlety: computing a symbol name does **not** drive codegen. It's a pure read. If you only hook `symbol_name` you'll get "undefined symbol" link errors — driving emission requires something like a `ReifyFnPointer` cast in a MIR body (4a).

Construct Instances with `Instance::new_raw(def_id, args)` (the plain `Instance::new` was renamed to force callers to confirm they're not accidentally constructing an unresolved Instance).

### 4c. `layout_of`

| | |
|---|---|
| **Key** | `PseudoCanonicalInput<'tcx, Ty<'tcx>>` |
| **Returns** | `Result<TyAndLayout<'tcx>, &'tcx LayoutError<'tcx>>` |
| **Fires** | extremely high volume — once for every type whose size/alignment rustc needs, including derived forms (`*mut T`, `&T`, `Option<T>`, tuples containing your type, etc.) |

Return a `LayoutData` describing the type's memory shape. `BackendRepr::Memory { sized: true }` tells rustc it's an opaque blob — no scalar-pair decomposition, no niche optimization.

`LayoutData`'s current fields include `fields` (a `FieldsShape` with `offsets` and `in_memory_order` — the latter renamed from `memory_index` during the layout-data cleanup), `backend_repr` (formerly `Abi`; see PR #132385), `largest_niche`, `uninhabited`, `align: AbiAlign` (the alignment wrapper struct), `size: Size`, `max_repr_align`, `unadjusted_abi_align`, and `randomization_seed: rustc_hashes::Hash64`.

Filter aggressively. You want to intercept `TyKind::Adt` only for specific types; intercepting derived forms corrupts their layouts and ICEs codegen. Check `has_param()` on the args to skip generic definitions that haven't been instantiated yet.

### 4d. `mir_shims`

| | |
|---|---|
| **Key** | `ty::InstanceKind<'tcx>` — variants include `DropGlue(DefId, Option<Ty<'tcx>>)`, `FnPtrShim(...)`, `CloneShim(...)` |
| **Returns** | `Body<'tcx>` |
| **Fires** | when rustc needs a generated shim body |

Pattern-match the `InstanceKind` and synthesize a MIR body. `DropGlue(_, Some(ty))` is the common target — you can return a body that calls a custom destructor.

### 4e. `collect_and_partition_mono_items`

| | |
|---|---|
| **Key** | `()` (singleton query) |
| **Returns** | `MonoItemPartitions<'tcx>` — a struct with `codegen_units: &'tcx [CodegenUnit<'tcx>]` and `all_mono_items: &'tcx DefIdSet` |
| **Fires** | once per compilation, the bridge between mono collection and codegen |

Delegate to the upstream provider, then destructure and reconstruct the returned partitions — filter items out, or mutate `MonoItemData` fields (linkage, visibility) on items that remain. The LLVM backend reads `data.linkage` directly from the CGU slice it receives; it doesn't re-query `mono_item_linkage_and_visibility`, so post-partition mutations survive to LLVM emission.

`Linkage` lives in `rustc_hir::attrs`. `Visibility` and `MonoItemData` live in `rustc_middle::mir::mono`. Getting the imports right is its own exercise; earlier nightlies had `Linkage` exported from the same module as the mono types.

Useful for removing items from rustc's codegen path or forcing specific linkage decisions without a source-level `#[linkage]` attribute.

### 4f. `upstream_monomorphizations_for`

| | |
|---|---|
| **Key** | `LocalDefId` |
| **Returns** | `Option<&'tcx UnordMap<GenericArgsRef<'tcx>, CrateNum>>` |
| **Fires** | during mono collection when deciding whether a generic monomorphization should be linked from an upstream rlib rather than emitted locally |

Return `None` to force local emission. Matters when generic items live in an rlib that doesn't contain the specific instantiation a downstream crate needs.

## Step 5: The codegen backend — `rustc_codegen_ssa::traits::CodegenBackend`

`make_codegen_backend` returns a `Box<dyn CodegenBackend>`. Rustc invokes its methods in a fixed order:

1. `name(&self) -> &'static str` — identifier for this backend (e.g., `"llvm"`).
2. `init(&Session)` — one-shot init.
3. `provide(&mut Providers)` — the backend can install its own query providers (separate from yours in step 4).
4. `codegen_crate(&self, tcx) -> Box<dyn Any>` — walks the monomorphized items and produces CGUs of machine code. Returns an opaque handle.
5. `join_codegen(ongoing, sess, outputs) -> (CodegenResults, WorkProducts)` — finalizes all codegen (blocks on any background threads), returns `CompiledModule`s + incremental work products.
6. `link(sess, codegen_results, metadata, outputs)` — invoke the linker. Note `metadata: EncodedMetadata` is a parameter here; earlier nightlies passed it through `codegen_crate`, but PR #141769 moved it to `link` (the phase that actually needs it).

The useful pattern is **wrap-and-delegate**: your backend holds `Box<dyn CodegenBackend>` pointing at rustc's real LLVM backend, forwards every method, and modifies inputs or outputs at whichever point you care about.

Two intervention points are especially handy:

- **In `join_codegen`**, append extra pre-compiled objects to `codegen_results.modules` as additional `CompiledModule` entries (with `ModuleKind::Regular`, `object: Some(path)`, and whatever other fields apply — including `links_from_incr_cache: Vec<_>` which the incremental system tracks). Rustc's linker picks them up alongside Rust-compiled modules.
- **Around `codegen_crate`**, run your own backend before or after rustc's.

`rustc_codegen_llvm::LlvmCodegenBackend::new()` is callable directly to construct the wrapped inner backend.

The trait was extracted from what used to be LLVM-specific code so Cranelift and GCC backends could plug in. An out-of-tree backend is loaded as a dylib exposing `__rustc_codegen_backend() -> Box<dyn CodegenBackend>` and selected via `-Zcodegen-backend=<path>`. The wrap-and-delegate pattern doesn't need any of that — it's just a trait impl in your crate.

## Step 6: Cargo-level interception — `RUSTC_WORKSPACE_WRAPPER`

Not a rustc extension point strictly speaking, but relevant. If you set `RUSTC_WORKSPACE_WRAPPER=/path/to/your-bin`, cargo invokes your binary instead of rustc for primary workspace crates, passing the real rustc path as argv[1] and the rustc args as argv[2..]. Non-workspace dependencies get invoked with vanilla rustc, untouched.

Useful environment variables cargo sets on the wrapped invocation:

- `CARGO_PRIMARY_PACKAGE` — set to `"1"` only for primary-workspace compiles
- `CARGO_MANIFEST_DIR` — the per-package source dir
- `CARGO_PKG_NAME` — the cargo package name
- `CARGO_TARGET_DIR` — the build output root (controllable from outside to share caches)

The wrapping binary typically inspects `argv[1]`'s basename for `rustc`, drops it, detects primary-vs-non-primary via the env var, and either delegates to plain rustc or dispatches into `run_compiler` with the custom callbacks.

## Step 7: The universal handle — `TyCtxt<'tcx>`

Every extension point either receives a `TyCtxt<'tcx>` directly or can get one through the query it's plugged into. `TyCtxt` is `Copy` but its `'tcx` is tied to arenas owned by the `run_compiler` stack frame — you cannot stash it in a `static` or smuggle it past the `Callbacks::after_*` return. All real work has to happen inside the callback or inside a `tcx.enter(|tcx| …)` closure.

`DefId` vs `LocalDefId` is the related distinction: a `DefId` is `(CrateNum, DefIndex)` and can point anywhere in the crate graph; `LocalDefId` is a `DefId` statically known to live in the local crate. Query keys pick one or the other to turn "this only applies to the local crate" from a runtime panic into a compile-time guarantee.

Useful TyCtxt methods:

- `tcx.item_name(def_id)`, `tcx.opt_item_name(def_id)` — item names
- `tcx.crate_name(crate_num)` — crate name for a `CrateNum`
- `tcx.def_kind(def_id)` — the `DefKind` (Fn, Struct, Trait, ...)
- `tcx.def_path(def_id)` — structural `DefPath`; safe from any phase
- `tcx.def_path_str(def_id)` — human-readable path; **only safe during diagnostics**, ICEs elsewhere
- `tcx.type_of(def_id)` — the item's type (as `EarlyBinder<Ty<'tcx>>`)
- `tcx.fn_sig(def_id)`, `tcx.fn_abi_of_instance(...)` — signature and ABI
- `tcx.layout_of(PseudoCanonicalInput { value, typing_env })` — type layout
- `tcx.module_children_local(def_id)` / `module_children(...)` — enumerate `pub use` re-exports and module contents, across crate boundaries
- `tcx.hir_crate_items(()).definitions()` / `.foreign_items()` — walk the local crate
- `tcx.arena` — the arena tied to `'tcx`

Query results that aren't trivially cloneable get arena-allocated: `tcx.arena.alloc(x)` returns `&'tcx T`, which is exactly the lifetime your provider's return type must name. Returning anything tied to a shorter lifetime is a compile error; use `tcx.arena.alloc_slice` or the `tcx.mk_*` interners for collections and interned values.

## Step 8: The threading contract

Queries fire on rayon worker threads. The lifecycle hooks (`config`, `after_analysis`) and the `CodegenBackend` methods run on whatever thread rustc's driver is on. Query providers fire concurrently, so any state shared across them needs proper synchronization.

`OnceLock` is the idiomatic choice for install-once immutable state (saved default providers, config). `Mutex` for anything mutable. Watch out for re-entrant locking — a query provider firing during a phase where your code already holds the lock will silently deadlock (0% CPU, hangs forever).

## Step 9: The nightly pin

Everything above is `#![feature(rustc_private)]` territory — no SemVer, APIs drift per nightly. Pin the toolchain (`rust-toolchain.toml` with a specific `channel = "nightly-YYYY-MM-DD"`) so drift is a conscious event.

Churn is uneven. Stable across nightlies: `rustc_interface::Config`, `rustc_driver::Callbacks`'s top-level shape, the existence of the `Providers` struct, the existence of `override_queries`, the `CodegenBackend` trait's role. Drifty per nightly: MIR construction primitives (struct fields, enum variants, non-exhaustive attributes), ABI helpers (`PassMode`, `BackendRepr`, `Linkage`'s module path), layout-data fields, individual query signatures. Budget ~1 week of adaptation per ~6 months of nightly gap.
