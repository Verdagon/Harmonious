# Plan: Split project into rustc-facade library + toylang consumer

## Context

The project currently mixes generic "integrate any language with rustc" code with toylang-specific code. The goal is to extract a reusable library that other toy compilers can build on, leaving toylang as one consumer.

## Background: How Types Get Registered With rustc

Before the query overrides ever fire, rustc needs to *know about* the consumer's types and functions. This happens through **stub injection**:

1. The consumer generates Rust source code (struct definitions + extern declarations)
2. A custom `FileLoader` intercepts rustc's file reading and returns this generated source
3. The consumer's test/app files do `mod __stubs; use __stubs::*;` to pull definitions in
4. rustc parses the stubs normally — assigns DefIds, runs type checking
5. Later, query overrides fire for those DefIds (layout_of, mir_built, etc.)

Every option below must account for this stub injection step. The question is who generates the stubs and who owns the FileLoader.

## Background: The Three-Phase Compilation Lifecycle

The consumer's compilation has three phases, each with different capabilities:

```
Phase 1: BEFORE RUSTC (consumer's frontend)
  - Parse source files
  - Type check (consumer's rules)
  - Produce generic IR (pre-monomorphization)
  - Build LangDef / registry / stubs
  - No tcx available — rustc hasn't started yet

Phase 2: DURING RUSTC, ON DEMAND (monomorphization)
  - Rustc's monomorphizer encounters a consumer function
  - Calls monomorphize() with concrete type args
  - Consumer instantiates its generic IR for those args
  - Consumer discovers which Rust generics it needs (rust_deps)
  - Consumer stashes the monomorphized body for Phase 3
  - tcx is available (passed as parameter)

Phase 3: DURING RUSTC, ONCE (codegen)
  - after_analysis fires — full tcx available
  - Consumer compiles all stashed monomorphized bodies to LLVM IR
  - Uses tcx for mangled symbol names, ABI coercion queries
  - Produces .o file injected into link step
```

Phase 1 *must* run before rustc because the library needs stubs and type definitions to start the rustc session. Phase 2 runs lazily inside rustc's monomorphization pass. Phase 3 runs once after rustc's analysis is complete.

Additionally, the current code identifies consumer items by **unqualified name** (e.g. `"Point"` not `"__toylang_stubs::Point"`), which is collision-prone. A better approach is for the library to track which DefIds came from the stub file and only dispatch to the consumer for those. Each option notes how it handles this.

## Current Coupling Analysis

### What the query overrides actually need from the consumer:

**layout.rs** needs:
- "Is this type name one of yours?" → bool
- "Give me the fields of this struct" → list of (name, field_type) where field_type maps to a Rust `Ty`
- "What are the type parameters?" → list of names

**borrowck.rs** needs:
- "Is this function name one of yours?" → bool

**mir_build.rs** needs:
- "Is this function name one of yours?" → bool
- "Monomorphize this function for these concrete type args" → MonomorphizeResult (includes extern symbol + Rust deps)

**drop_glue.rs** needs:
- "Is this type name one of yours?" → bool

**callbacks.rs / main.rs** need:
- Pass the registry to query overrides
- Call the consumer's codegen in `after_analysis`
- Call the consumer's stub generation

---

## Option A: Thin Trait — "Existence Checks + Field Iteration"

The library defines a minimal trait. The consumer handles stub generation and FileLoader setup externally.

### Full API surface

```rust
// ===== Library crate: rustc-lang-facade =====

/// The main interface between the library and a consumer language.
/// Must be Send + Sync because rustc query providers run on Rayon worker threads.
pub trait LangRegistry: Send + Sync {
    /// Does this name correspond to a type defined in the consumer's language?
    /// Called by layout_of and drop_glue overrides when rustc encounters an ADT name.
    fn is_lang_type(&self, name: &str) -> bool;

    /// Does this name correspond to a function defined in the consumer's language?
    /// Called by mir_built and borrowck overrides to decide whether to intercept.
    fn is_lang_fn(&self, name: &str) -> bool;

    /// Monomorphize a consumer type for concrete type args.
    /// Called from the layout_of override when rustc needs the layout of a
    /// consumer type (possibly a generic instantiation like MyStruct<i32>).
    ///
    /// The consumer substitutes any type parameters with concrete types and
    /// returns the field types for this specific instantiation. The library
    /// calls tcx.layout_of() on each field type to compute the struct layout.
    ///
    /// The consumer can also return rust_deps — e.g. if the struct contains
    /// a Vec<i32> field, it may want to trigger monomorphization of Vec<i32>
    /// or its associated methods.
    ///
    /// The concrete type arguments are available via the ADT's generic args
    /// in the Ty passed to layout_of.
    fn monomorphize_type(&self, name: &str, tcx: TyCtxt, def_id: LocalDefId)
        -> MonomorphizeTypeResult;

    /// Monomorphize a consumer function for concrete type args.
    /// Called from the mir_built override when rustc's monomorphizer
    /// encounters a consumer function.
    ///
    /// This is the hook point for the consumer's own monomorphizer. The flow:
    /// 1. Rustc's monomorphizer wants MIR for e.g. `wrap<i32>`
    /// 2. The library's mir_built override calls this method
    /// 3. The consumer monomorphizes its generic IR for `wrap` with `T = i32`
    /// 4. During that, the consumer discovers it needs `Vec::push<i32>`
    /// 5. The consumer picks a symbol name for the monomorphized body
    /// 6. The consumer returns MonomorphizeFnResult with the symbol + deps
    /// 7. The library builds a MIR call stub targeting that symbol,
    ///    with phantom casts for the rust deps
    /// 8. Rustc's monomorphizer sees the casts → monomorphizes `Vec::push<i32>`
    ///
    /// The concrete type arguments are available via `tcx.fn_sig(def_id)` on
    /// the monomorphized signature. The consumer should also stash the
    /// monomorphized body for later codegen in after_analysis.
    fn monomorphize_fn(&self, name: &str, tcx: TyCtxt, def_id: LocalDefId)
        -> MonomorphizeFnResult;
}

/// Result of monomorphizing a consumer type for a specific set of type args.
pub struct MonomorphizeTypeResult {
    /// The concrete field types for this instantiation, in declaration order.
    /// The library calls tcx.layout_of() on each to compute struct layout.
    /// E.g. for MyStruct<i32>: field_types might be [tcx.types.i32, Vec<i32>].
    pub field_types: Vec<Ty>,
    /// Rust generic instantiations (types or functions) this type depends on.
    /// E.g. if the struct contains a Vec<i32> field, the consumer might want
    /// to trigger monomorphization of Vec<i32>'s drop glue or methods.
    pub rust_deps: Vec<(DefId, GenericArgsRef)>,
}

/// Result of monomorphizing a consumer function for a specific set of type args.
pub struct MonomorphizeFnResult {
    /// The extern symbol name for this monomorphized function.
    /// The library builds a MIR call stub that calls this symbol.
    /// E.g. "__mylang_impl_make_counter" or "__mylang_impl_wrap_i32".
    pub extern_symbol: String,
    /// Rust generic instantiations (types or functions) this body depends on.
    /// The library emits phantom casts in the MIR stub so rustc's
    /// monomorphizer will stamp these out. Can include both Rust function
    /// instantiations (e.g. Vec::push<i32>) and Rust type instantiations
    /// (e.g. HashMap<MyKey, MyValue>).
    pub rust_deps: Vec<(DefId, GenericArgsRef)>,
}

/// Install query overrides for a consumer language.
/// Call this from your Callbacks::config() implementation.
pub fn install_overrides(
    registry: Arc<dyn LangRegistry>,
    providers: &mut Providers,
);

// The library also re-exports these helper modules (see Option D for full descriptions):

/// MIR body construction: call stubs, phantom deps, drop glue, constants.
pub mod mir_helpers;
/// ABI coercion queries: detect scalar-coerced struct returns.
pub mod abi_helpers;
/// Rustc type system lookups: find structs, Vec methods, allocator types.
pub mod oracle;
/// CodegenBackend wrapper: inject external .o into the link step.
pub mod codegen_wrapper;
/// Custom FileLoader: inject synthesized Rust stubs as a virtual file.
pub mod file_loader;
```

### What the consumer must do

```rust
// ===== Consumer crate: toylangc =====

// 1. Implement the trait
impl LangRegistry for ToylangRegistry { ... }

// 2. Generate stub source code (consumer's responsibility)
let stubs: String = my_stub_gen::generate(&registry);

// 3. Set up the FileLoader (using the library's helper)
let file_loader = rustc_lang_facade::file_loader::LangFileLoader::new(
    "__toylang_stubs.rs",  // virtual file path
    stubs,                  // generated Rust source
);

// 4. In Callbacks::config(), install overrides and file loader
fn config(&mut self, config: &mut Config) {
    config.file_loader = Some(Box::new(file_loader));
    rustc_lang_facade::install_overrides(
        Arc::new(self.registry.clone()),
        &mut config.override_queries,
    );
}

// 5. In Callbacks::after_analysis(), do your own codegen
fn after_analysis(&mut self, tcx: TyCtxt) {
    let (ir, symbols) = my_llvm_gen::generate(tcx, &self.registry);
    // compile IR, set up codegen wrapper, etc.
}
```

### Stub identification

The library matches by unqualified name (same as today). The consumer is responsible for avoiding name collisions. A future improvement could have `install_overrides` accept a module path prefix to scope the matching.

### Pros
- Small trait surface (~4 methods)
- Query overrides become fully generic
- Consumer keeps full control over stubs, codegen, FileLoader setup
- No shared data types — consumer returns rustc `Ty` directly from monomorphize_type

### Cons
- `monomorphize_type` and `monomorphize_fn` force consumer to use `tcx` and rustc imports
- Consumer must wire up FileLoader, CodegenBackend wrapper, and after_analysis manually
- Name-based matching is collision-prone

---

## Option B: Callback-Based — "Don't Define Types, Ask Questions"

The library never defines field/type data types. Instead, it asks the consumer everything via callbacks. The library owns the full driver lifecycle including FileLoader and CodegenBackend setup.

### Full API surface

```rust
// ===== Library crate: rustc-lang-facade =====

/// All-callback interface. The library never defines its own field/type types —
/// it just asks the consumer questions when it needs answers.
///
/// The library identifies consumer items automatically by tracking which DefIds
/// came from the stub file (injected via generate_stubs). No is_lang_type /
/// is_lang_fn methods needed — the library already knows.
pub trait LangCallbacks: Send + Sync {
    // --- Stub injection: called before rustc parsing begins (Phase 1) ---

    /// Generate the Rust source code to inject via FileLoader.
    /// Must contain struct definitions and extern "C" declarations that
    /// make the consumer's types visible to rustc's type checker.
    /// The library handles FileLoader setup — the consumer just returns the source.
    fn generate_stubs(&self) -> String;

    // --- Monomorphization: called on demand during Phase 2 ---

    /// Monomorphize a consumer type for concrete type args.
    /// Called from layout_of when rustc needs the layout of a consumer type.
    /// See Option A's monomorphize_type for details.
    /// Returns MonomorphizeTypeResult with field_types + rust_deps.
    fn monomorphize_type(&self, type_name: &str, tcx: TyCtxt, def_id: LocalDefId)
        -> MonomorphizeTypeResult;

    /// Monomorphize a consumer function for concrete type args.
    /// Called from mir_built when rustc's monomorphizer encounters a consumer fn.
    /// See Option A's monomorphize_fn for details.
    /// Returns MonomorphizeFnResult with extern_symbol + rust_deps.
    fn monomorphize_fn(&self, fn_name: &str, tcx: TyCtxt, def_id: LocalDefId)
        -> MonomorphizeFnResult;

    // --- Codegen: called from after_analysis hook (Phase 3) ---

    /// Compile the consumer's function bodies and return the path to the .o file.
    /// Called during after_analysis when the full TyCtxt is available for
    /// symbol name resolution, ABI queries, etc. Return None if no codegen needed.
    fn generate_and_compile(&self, tcx: TyCtxt) -> Option<PathBuf>;
}

/// Entry point. The library owns the entire driver lifecycle:
/// - Sets up FileLoader with stubs from generate_stubs()
/// - Installs query overrides that dispatch to the callbacks
/// - Wraps CodegenBackend to inject the .o from generate_and_compile()
/// - Runs rustc
pub fn run_compiler(
    callbacks: Arc<dyn LangCallbacks>,
    rustc_args: &[String],
) -> Result<(), Error>;

// Helper modules re-exported for consumers that need lower-level access
// (e.g. for implementing generate_and_compile during Phase 3):

/// MIR body construction: call stubs, phantom deps, drop glue, constants.
pub mod mir_helpers;
/// ABI coercion queries: detect scalar-coerced struct returns.
pub mod abi_helpers;
/// Rustc type system lookups: find structs, Vec methods, allocator types.
pub mod oracle;
```

### What the consumer must do

```rust
// ===== Consumer crate: toylangc =====

impl LangCallbacks for Toylang {
    fn generate_stubs(&self) -> String { my_stub_gen::generate(self) }
    fn monomorphize_type(&self, name: &str, tcx: TyCtxt, def_id: LocalDefId) -> MonomorphizeTypeResult { ... }
    fn monomorphize_fn(&self, name: &str, tcx: TyCtxt, def_id: LocalDefId) -> MonomorphizeFnResult { ... }
    fn generate_and_compile(&self, tcx: TyCtxt) -> Option<PathBuf> { ... }
}

fn main() {
    let toylang = Arc::new(Toylang::from_source("input.toylang"));
    rustc_lang_facade::run_compiler(toylang, &rustc_args).unwrap();
}
```

### Stub identification

The library tracks which DefIds came from the stub file it injected. Only those DefIds trigger callbacks — no name collision risk.

### Pros
- No shared data types between library and consumer
- Consumer has complete freedom in its internal representation
- `field_types` lets the consumer map its types to `Ty` directly — no intermediate enum
- Library owns the full driver lifecycle — consumer just implements one trait
- `generate_stubs` makes stub injection explicit in the API
- No `is_lang_type`/`is_lang_fn` needed — library knows from the stub file

### Cons
- `monomorphize_type` and `monomorphize_fn` take `tcx` → lifetime issues with `TyCtxt<'tcx>`
- Consumer still needs rustc imports for `Ty`, `TyCtxt`, `DefId`

---

## Option C: Two-Phase — "Registry struct + Codegen trait"

Split the interface into a data-only struct (for layout/borrowck/drop — no tcx needed) and a trait (for codegen/MIR — needs tcx). The library owns the driver lifecycle.

### Full API surface

```rust
// ===== Library crate: rustc-lang-facade =====

// =======================================================================
// Phase 1: Plain data listing the consumer's type and function names.
// Used by the library to identify which DefIds belong to the consumer.
// No field/layout info here — that comes from monomorphize_type at
// query time, since generic types need concrete args to resolve fields.
// =======================================================================

/// All type and function names from the consumer's language.
/// Passed to the library's run_compiler() and stored in a global OnceLock
/// for query providers running on Rayon worker threads.
pub struct LangDef {
    /// Consumer-defined struct type names.
    /// The library uses these to identify consumer types in layout_of
    /// and drop_glue overrides.
    pub type_names: HashSet<String>,
    /// Consumer-defined function names.
    /// The library uses these to identify consumer functions in
    /// mir_built and borrowck overrides.
    pub fn_names: HashSet<String>,
}

// =======================================================================
// Phase 2: Trait for operations that need access to rustc's TyCtxt.
// =======================================================================

/// Codegen callbacks implemented by the consumer.
pub trait LangCodegen: Send + Sync {
    /// Generate Rust source code for the stub file injected via FileLoader.
    /// Must contain struct definitions and extern "C" declarations that
    /// make the consumer's types visible to rustc's type checker.
    /// Called once before rustc parsing begins (Phase 1).
    fn generate_stubs(&self, defs: &LangDef) -> String;

    /// Monomorphize a consumer type for concrete type args.
    /// Called from layout_of when rustc needs the layout of a consumer type.
    /// See Option A's monomorphize_type for details.
    /// Returns MonomorphizeTypeResult with field_types + rust_deps.
    fn monomorphize_type(&self, type_name: &str, tcx: TyCtxt, def_id: LocalDefId)
        -> MonomorphizeTypeResult;

    /// Monomorphize a consumer function for concrete type args.
    /// Called from mir_built when rustc's monomorphizer encounters a consumer fn.
    /// See Option A's monomorphize_fn for details.
    /// Returns MonomorphizeFnResult with extern_symbol + rust_deps.
    fn monomorphize_fn(&self, fn_name: &str, tcx: TyCtxt, def_id: LocalDefId)
        -> MonomorphizeFnResult;

    /// Called during after_analysis when the full TyCtxt is available (Phase 3).
    /// The consumer should compile its function bodies here (e.g. to LLVM IR),
    /// using tcx for symbol name resolution and ABI queries.
    /// Returns the path to the compiled .o file to inject into the link step,
    /// or None if no external codegen is needed.
    fn after_analysis(&self, tcx: TyCtxt, defs: &LangDef) -> Option<PathBuf>;
}

/// Entry point. The library owns the entire driver lifecycle:
/// - Calls codegen.generate_stubs() and sets up FileLoader
/// - Installs query overrides that check defs for consumer items
/// - Calls codegen.monomorphize_type() from the layout_of override
/// - Calls codegen.monomorphize_fn() from the mir_built override
/// - Calls codegen.after_analysis() and wraps CodegenBackend to inject the .o
/// - Runs rustc
pub fn run_compiler(
    defs: Arc<LangDef>,
    codegen: Arc<dyn LangCodegen>,
    rustc_args: &[String],
) -> Result<(), Error>;

// Helper modules re-exported for consumers (used in Phase 2 and 3):

/// MIR body construction: call stubs, phantom deps, drop glue, constants.
pub mod mir_helpers;
/// ABI coercion queries: detect scalar-coerced struct returns.
pub mod abi_helpers;
/// Rustc type system lookups: find structs, Vec methods, allocator types.
pub mod oracle;
```

### What the consumer must do

```rust
// ===== Consumer crate: toylangc =====

// 1. Build LangDef — just the names
fn build_lang_def(registry: &ToylangRegistry) -> LangDef {
    LangDef {
        type_names: registry.structs.keys().cloned().collect(),
        fn_names: registry.functions.keys().cloned().collect(),
    }
}

// 2. Implement the codegen trait
impl LangCodegen for ToylangCodegen {
    fn generate_stubs(&self, defs: &LangDef) -> String { ... }
    fn monomorphize_type(&self, name: &str, tcx: TyCtxt, def_id: LocalDefId) -> MonomorphizeTypeResult { ... }
    fn monomorphize_fn(&self, name: &str, tcx: TyCtxt, def_id: LocalDefId) -> MonomorphizeFnResult { ... }
    fn after_analysis(&self, tcx: TyCtxt, defs: &LangDef) -> Option<PathBuf> { ... }
}

// 3. Call the library
fn main() {
    let registry = parse_toylang("input.toylang");
    let defs = Arc::new(build_lang_def(&registry));
    let codegen = Arc::new(ToylangCodegen::new(registry));
    rustc_lang_facade::run_compiler(defs, codegen, &rustc_args).unwrap();
}
```

### Stub identification

The library owns the FileLoader and knows which virtual file it injected. It can record the DefIds parsed from that file and only consult `defs.type_names`/`defs.fn_names` for those DefIds. This eliminates name collision risk without any consumer effort.

### Pros
- Clean separation: names (LangDef) vs behavior (LangCodegen)
- `LangDef` is trivially simple — just two sets of strings
- `LangCodegen` is a small trait (4 methods)
- The `'tcx` lifetime only appears in `LangCodegen`, not in LangDef
- Library can own DefId tracking for safe item identification
- No `LangFieldType` enum — consumer returns rustc `Ty` directly from monomorphize_type, so any field type Rust supports is possible

### Cons
- `monomorphize_type` and `monomorphize_fn` take `tcx` → lifetime issues with `TyCtxt<'tcx>`
- Consumer still needs rustc imports for `Ty`, `TyCtxt`, `DefId`

---

## Option D: Minimal — "Just Extract the Helpers"

Don't abstract the query overrides at all. Just extract the genuinely generic utilities into a library crate. The consumer copies/adapts the query override code for its own types.

### Full API surface

```rust
// ===== Library crate: rustc-lang-helpers =====
// No traits, no framework — just reusable building blocks.

/// MIR body construction utilities.
/// build_extern_call_body: creates a stub that calls an extern "C" symbol.
/// build_phantom_call_body: same, but adds ReifyFnPointer casts to trigger
///   rustc's monomorphizer for Rust generic instantiations.
/// build_const_i32_body: trivial body returning a constant (for testing).
/// build_drop_call_body: drop glue that calls a consumer's destructor symbol.
pub mod mir_helpers;

/// ABI query wrapper.
/// coerced_return_type: queries fn_abi_of_instance to determine if rustc
///   coerces a struct return to a scalar (e.g. {i32,i32} → i64 on aarch64).
/// Returns CoercedReturn::Direct/Indirect/Void.
pub mod abi_helpers;

/// rustc type system query helpers.
/// find_local_struct_ty: look up a struct by name in the HIR.
/// find_vec_method: find Vec::new/push/len DefIds.
/// extract_global_ty: get the Global allocator type from Vec's generic args.
pub mod oracle;

/// CodegenBackend wrapper that injects an external .o file into
/// rustc's CodegenResults during join_codegen, so it participates
/// in the final link step without modifying rustc's codegen pipeline.
pub mod codegen_wrapper;

/// Custom FileLoader that intercepts rustc's file reading to inject
/// synthesized Rust source code (struct defs, extern declarations)
/// as a virtual file, avoiding the need for a real .rs stub on disk.
pub mod file_loader;
```

### What the consumer must do

The consumer writes all the glue code that Options A-C abstract away. This is essentially what exists today in the project, minus the helper modules that move into the library.

```rust
// ===== Consumer crate: toylangc =====
// Depends on: rustc_lang_helpers (the library), plus rustc_driver, rustc_middle, etc.

// ---------------------------------------------------------------------------
// Phase 1: Before rustc — parse source, build registry, generate stubs
// ---------------------------------------------------------------------------

fn main() {
    // Parse consumer's source files into an internal registry
    let registry = my_parser::parse("input.mylang");

    // Generate Rust stub source (struct defs + extern declarations)
    let stubs: String = my_stub_gen::generate(&registry);

    // Prepare rustc args (same as what you'd pass to rustc)
    let args = vec![
        "myfile.rs".to_string(),
        "--edition".to_string(), "2021".to_string(),
        "-o".to_string(), "/tmp/output".to_string(),
    ];

    // Wrap registry in Arc for sharing across Rayon threads
    let registry = Arc::new(registry);

    // Run the rustc driver with our callbacks
    rustc_driver::RunCompiler::new(&args, &mut MyCallbacks {
        registry: registry.clone(),
        stubs,
    }).run().unwrap();
}

// ---------------------------------------------------------------------------
// Callbacks: wire up FileLoader, query overrides, and codegen
// ---------------------------------------------------------------------------

struct MyCallbacks {
    registry: Arc<MyRegistry>,
    stubs: String,
}

impl rustc_driver::Callbacks for MyCallbacks {
    fn config(&mut self, config: &mut Config) {
        // Inject stub source via the library's FileLoader helper
        config.file_loader = Some(Box::new(
            rustc_lang_helpers::file_loader::LangFileLoader::new(
                "__my_stubs.rs",
                self.stubs.clone(),
            )
        ));

        // Install query overrides — consumer writes these (see below)
        let registry = self.registry.clone();
        config.override_queries = Some(Box::new(move |_sess, providers| {
            // Save defaults before overriding
            my_queries::layout::save_default(providers.layout_of);
            my_queries::mir_build::save_default(providers.mir_built);
            my_queries::borrowck::save_default(providers.mir_borrowck);
            my_queries::drop_glue::save_default(providers.mir_shims);

            // Install our overrides
            my_queries::layout::install_registry(registry.clone());
            my_queries::mir_build::install_registry(registry.clone());
            my_queries::borrowck::install_registry(registry.clone());
            my_queries::drop_glue::install_registry(registry.clone());

            providers.layout_of = my_queries::layout::my_layout_of;
            providers.mir_built = my_queries::mir_build::my_mir_built;
            providers.mir_borrowck = my_queries::borrowck::my_mir_borrowck;
            providers.mir_shims = my_queries::drop_glue::my_mir_shims;
        }));

        // Wrap the codegen backend to inject our .o file at link time
        config.make_codegen_backend = Some(Box::new(|_| {
            Box::new(rustc_lang_helpers::codegen_wrapper::LangCodegenBackend::new())
        }));
    }

    fn after_analysis(&mut self, tcx: TyCtxt) -> bool {
        // Phase 3: compile consumer function bodies using tcx
        let (llvm_ir, rust_symbols) = my_llvm_gen::generate_with_tcx(tcx, &self.registry);
        let obj_path = my_llvm_gen::compile_to_object(&llvm_ir);

        // Tell the codegen wrapper where the .o file is
        rustc_lang_helpers::codegen_wrapper::set_object_path(obj_path);
        rustc_lang_helpers::codegen_wrapper::set_global_symbols(rust_symbols);

        true // continue compilation
    }
}

// ---------------------------------------------------------------------------
// Query overrides: consumer writes these, using library helpers
// ---------------------------------------------------------------------------

// layout_of override — compute struct layout for consumer types
fn my_layout_of(tcx: TyCtxt, query: PseudoCanonicalInput<Ty>) -> Result<TyAndLayout, ...> {
    if let TyKind::Adt(adt_def, args) = query.value.kind() {
        let name = tcx.item_name(adt_def.did()).to_string();
        if let Some(my_struct) = REGISTRY.get().and_then(|r| r.structs.get(&name)) {
            // Compute field offsets using tcx.layout_of() for each field type
            // (same pattern as current queries/layout.rs)
            return Ok(build_layout(tcx, query.value, my_struct, query.typing_env));
        }
    }
    DEFAULT_LAYOUT_OF(tcx, query)  // fall through to rustc's default
}

// mir_built override — build MIR call stubs for consumer functions
fn my_mir_built(tcx: TyCtxt, def_id: LocalDefId) -> &Steal<Body> {
    let name = tcx.opt_item_name(def_id.to_def_id());
    if REGISTRY.get().map_or(false, |r| r.functions.contains_key(&name)) {
        // Phase 2: monomorphize — consumer picks the symbol name and discovers deps
        let result = my_monomorphize(tcx, def_id);

        // Use library helper to build the MIR stub
        if result.rust_deps.is_empty() {
            return rustc_lang_helpers::mir_helpers::build_extern_call_body(
                tcx, def_id, &result.extern_symbol,
            );
        } else {
            return rustc_lang_helpers::mir_helpers::build_phantom_call_body(
                tcx, def_id, &result.extern_symbol, &result.rust_deps,
            );
        }
    }
    DEFAULT_MIR_BUILT(tcx, def_id)  // fall through to rustc's default
}

// borrowck override — skip borrow checking for consumer functions
fn my_mir_borrowck(tcx: TyCtxt, def_id: LocalDefId) -> &BorrowCheckResult {
    if REGISTRY.get().map_or(false, |r| r.functions.contains_key(&name)) {
        // Return empty borrow check result — consumer's type checker
        // already verified safety
        return tcx.arena.alloc(BorrowCheckResult { ... });
    }
    DEFAULT_MIR_BORROWCK(tcx, def_id)
}

// mir_shims override — provide drop glue for consumer types
fn my_mir_shims(tcx: TyCtxt, instance: InstanceKind) -> &Body {
    if let InstanceKind::DropGlue(_, Some(ty)) = instance {
        if is_my_type(tcx, ty) {
            // Use library helper to build drop call body
            return rustc_lang_helpers::mir_helpers::build_drop_call_body(
                tcx, ty, &format!("__mylang_drop_{}", type_name),
            );
        }
    }
    DEFAULT_MIR_SHIMS(tcx, instance)
}
```

### Stub identification

Consumer's responsibility entirely. They write their own query overrides and decide how to identify their items (by name, by DefId, by module path, etc.).

### Pros
- Simplest to implement — just move files into a separate crate
- No trait design needed
- No abstraction leaks — consumer sees exactly what's happening
- Helpers are genuinely reusable without any adaptation
- No lifetime issues
- Maximum flexibility — consumer can customize anything

### Cons
- Consumer must write its own query override boilerplate (~200 lines of careful code)
- The "hard part" (wiring overrides correctly, avoiding gotchas) isn't shared
- Each new consumer rediscovers the same pitfalls (adt-only filtering, StorageLive/Dead, etc.)
- No stub injection guidance

---

## Option E: Framework — "Library Owns the Driver"

The library provides a complete `run_compiler` function. The consumer never imports rustc crates — it just fills in a config struct with plain data. Stubs, FileLoader, query overrides, and CodegenBackend wrapping are all handled internally.

### Full API surface

```rust
// ===== Library crate: rustc-lang-facade =====
// The consumer never imports rustc crates — just fills in this config.

/// Everything the library needs to run a compilation.
/// The consumer prepares this before calling compile().
pub struct LangConfig {
    /// Type definitions from the consumer's language.
    pub types: Vec<TypeDef>,
    /// Function definitions from the consumer's language.
    pub functions: Vec<FnDef>,
    /// Rust source code to inject via FileLoader. Contains struct definitions
    /// and extern "C" declarations. The consumer generates this from its own
    /// type/function definitions before calling compile().
    pub stub_source: String,
    /// Path to a pre-compiled .o file containing the consumer's function
    /// implementations. Injected into the link step. None if the consumer
    /// doesn't have externally compiled code (e.g. pure MIR generation).
    pub object_file: Option<PathBuf>,
}

/// A struct type defined in the consumer's language.
pub struct TypeDef {
    /// Type name as it appears in the Rust stubs (e.g. "Point", "Pair").
    pub name: String,
    /// Generic type parameter names (e.g. ["A", "B"]).
    pub type_params: Vec<String>,
    /// Fields in declaration order.
    pub fields: Vec<FieldDef>,
}

/// A single field in a consumer-defined struct.
pub struct FieldDef {
    /// Field name.
    pub name: String,
    /// Rust-equivalent type of this field, for layout computation.
    pub ty: FieldType,
}

/// Field type enum. Same as Option C's LangFieldType.
pub enum FieldType { I32, I64, F64, Bool, TypeParam(String) }

/// A function defined in the consumer's language.
pub struct FnDef {
    /// Function name as it appears in the Rust stubs.
    pub name: String,
    /// The extern symbol name for the MIR call stub (e.g. "__mylang_impl_foo").
    /// The library builds a MIR stub that calls this symbol.
    pub extern_symbol: String,
    /// Pre-computed Rust generic dependencies. The library uses these to emit
    /// phantom monomorphization triggers. Pre-computed because the consumer
    /// doesn't have access to tcx in this model — the library resolves
    /// these to DefIds internally using the oracle.
    /// NOTE: Unlike Options A-C where the consumer's monomorphizer runs
    /// on-demand inside rustc's monomorphization pass (via the monomorphize
    /// method), here the deps are fixed at config construction time. This
    /// means generic consumer functions whose deps vary by type argument
    /// cannot be supported.
    pub rust_deps: Vec<RustDep>,
}

/// A Rust generic instantiation that must be monomorphized.
pub struct RustDep {
    /// Which Rust API is being used.
    pub kind: RustDepKind,
    /// Name of the consumer-defined struct used as the type argument
    /// (e.g. "Point" for Vec<Point>).
    pub element_type: String,
}

/// Known Rust API patterns. The library resolves these to DefIds internally.
/// This is the main extensibility limitation — new patterns require library changes.
pub enum RustDepKind { VecNew, VecPush, VecLen }

/// Entry point. Sets up the rustc driver with query overrides, injects stubs
/// via FileLoader, wraps CodegenBackend to inject the .o file, and runs
/// compilation to produce the final binary.
pub fn compile(config: LangConfig, rustc_args: &[String]) -> Result<(), Error>;
```

### What the consumer must do

```rust
// ===== Consumer crate: toylangc =====

fn main() {
    let registry = parse_toylang("input.toylang");

    // 1. Compile to LLVM IR and produce .o file (BEFORE calling the library)
    //    ⚠ This is the fundamental problem — see note below.
    let obj_path = my_llvm_gen::compile_to_object(&registry);

    // 2. Generate stub source (consumer knows its own types)
    let stubs = my_stub_gen::generate(&registry);

    // 3. Build config with plain data
    let config = LangConfig {
        types: registry.structs.iter().map(|s| TypeDef { ... }).collect(),
        functions: registry.functions.iter().map(|f| FnDef {
            name: f.name.clone(),
            extern_symbol: format!("__mylang_impl_{}", f.name),
            rust_deps: scan_for_vec_ops(&f.body),  // consumer pre-computes deps
        }).collect(),
        stub_source: stubs,
        object_file: Some(obj_path),
    };

    // 4. One function call — library handles everything else
    rustc_lang_facade::compile(config, &rustc_args).unwrap();
}
```

**Fundamental limitation:** The consumer compiles to LLVM IR in step 1, *before* rustc starts. This means:
- **No `tcx`** — can't call `tcx.symbol_name(instance)` for mangled Rust symbols
- **No monomorphization** — can't know which concrete type args rustc will request, so generic consumer functions can't be compiled
- **No ABI queries** — can't call `fn_abi_of_instance` to detect coerced returns

The consumer would need to use un-mangled `extern "C"` wrappers with fixed signatures, no generics, and no ABI coercion. This effectively limits Option E to languages with no generics that only call Rust functions through C-ABI wrappers — a much simpler (and less useful) integration than Options A-C.

### Stub identification

The library owns all of it. It parses the `stub_source`, knows the resulting DefIds, and matches them against `config.types`/`config.functions` by name. Since the library controls both the FileLoader and the query overrides, it can use DefId-based matching internally.

### Pros
- Simplest consumer experience — fill in a struct, call one function
- No traits, no lifetimes, no rustc imports needed by consumer
- All rustc complexity hidden behind one function call
- Library handles stub injection, query overrides, codegen wrapping, everything

### Cons
- **Codegen happens before rustc** — no tcx, no mangled symbols, no ABI queries, no generics
- `RustDepKind` enum is hardcoded to Vec operations — new Rust APIs require library changes
- No `monomorphize` hook — consumer can't participate in rustc's monomorphization pass
- Consumer can't customize query behavior
- Most rigid option — effectively limited to non-generic, C-ABI-only integrations

---

## Summary

| | Stubs | Query overrides | Monomorphize hook | Codegen | Consumer needs rustc? | Trait methods |
|---|---|---|---|---|---|---|
| **A** | Consumer generates + wires FileLoader | Library (via trait) | `monomorphize_type()` + `monomorphize_fn()` on trait | Consumer wires after_analysis | Yes | ~4 |
| **B** | Consumer generates, library wires | Library (via callbacks) | `monomorphize_type()` + `monomorphize_fn()` on trait | Library calls callback | Yes | ~4 |
| **C** | Consumer generates, library wires | Library (reads LangDef) | `monomorphize_type()` + `monomorphize_fn()` on trait | Library calls trait | Yes (only in LangCodegen) | 4 |
| **D** | Consumer does everything | Consumer (using helpers) | Consumer writes own | Consumer does everything | Yes | 0 |
| **E** | Consumer generates, library wires | Library (reads config) | None (pre-computed) | Before rustc (no tcx) | No | 0 |

## Recommendation

**Options B and C have converged** — both now have ~4 trait methods with the same signatures (`monomorphize_type`, `monomorphize_fn`, `generate_stubs`, `generate_and_compile`/`after_analysis`). The only difference is that C has a `LangDef` with name sets for identification, while B relies on DefId tracking from the stub file.

**Option B or C** is the sweet spot:

- Consumer returns rustc `Ty` directly from `monomorphize_type` — no `LangFieldType` enum, so any field type Rust supports works (including other consumer types, Rust generics, arrays, etc.)
- `monomorphize_fn` is the natural hook for the consumer's monomorphizer
- `LangCodegen` trait is small (4 methods)
- Library owns the driver lifecycle
- The `'tcx` lifetime is contained to trait methods that genuinely need it

**Option D (Minimal)** is worth considering if you want to ship something quickly and iterate on the abstraction later. Extract the helpers now, design the framework after a second consumer validates the right abstraction boundary.
