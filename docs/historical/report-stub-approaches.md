# Report: How to Express Stub Definitions to rustc

## The Problem

Before any query overrides fire (layout_of, mir_built, etc.), rustc must *know* that types like `Point` and functions like `make_vec` exist. It needs to assign them `DefId`s, check that code using them type-checks, and include them in monomorphization. The question is: how do we tell rustc about these definitions?

## What Happens Today

The current approach generates **literal Rust source text** and injects it via a custom `FileLoader`.

### The generated source

For `counter.toylang`:
```rust
pub struct Counter {
    pub value: i32,
}
extern "C" {
    pub fn __toylang_impl_make_counter() -> Counter;
    pub fn __toylang_impl_wrap_value(x: i32) -> Counter;
}
```

For `pair.toylang` (generic):
```rust
pub struct Pair<A, B> {
    pub first: A,
    pub second: B,
}
extern "C" {
    pub fn __toylang_impl_make_pair() -> Pair<i32, i32>;
}
```

For `point.toylang` (uses Vec, so has phantom deps):
```rust
pub struct Point {
    pub x: i32,
    pub y: i32,
}
extern "C" {
    pub fn __toylang_impl_make_vec(_dep0: *const (), _dep1: *const ()) -> Vec<Point>;
    pub fn __toylang_impl_vec_len(v: &Vec<Point>, _dep0: *const ()) -> usize;
}
```

### How it gets to rustc

1. `stub_gen::generate()` builds this string from the registry
2. `ToylangFileLoader` implements rustc's `FileLoader` trait
3. When rustc tries to read `__toylang_stubs.rs`, the FileLoader returns the string
4. Test files contain `mod __toylang_stubs; use __toylang_stubs::*;`
5. rustc parses the stubs through its normal frontend — lexer, parser, HIR, type checking
6. DefIds are assigned automatically during parsing
7. Later, query overrides fire for these DefIds

### What's good about this

- **Correctness is free.** rustc's own parser validates that the generated source is syntactically valid. Its own type checker validates that the types are consistent. If the stub has a bug (e.g., wrong field type), rustc catches it at parse time with a clear error.
- **Full type system integration.** The stub types participate in all of rustc's machinery — trait resolution, monomorphization, coherence checking, error messages. They're real Rust types.
- **Resilient to nightly changes.** rustc's parser is one of its most stable components. Source syntax rarely changes in breaking ways. By contrast, HIR node constructors and metadata formats change frequently.
- **Simple to debug.** You can print the generated source and read it. It's just Rust.

### What's not great

- **String concatenation is fragile.** `stub_gen.rs` builds source with `format!()` and `push_str()`. A missing comma or brace produces a parse error inside rustc that's confusing to diagnose because it points at a virtual file.
- **Limited expressiveness.** Some things are awkward to express as source text — e.g., setting specific attributes, controlling field ordering, or defining complex trait impls.
- **Redundancy.** The consumer's type information exists in two forms: the registry data structure and the generated source text. They must stay in sync.
- **No `#[repr(C)]`.** The stubs currently use default Rust repr, which means rustc is free to reorder fields. The layout_of override computes a layout that assumes declaration order, but rustc's own layout for the "same" type could differ. This works today because the layout_of override *replaces* rustc's layout entirely, but it's a subtle correctness requirement.

---

## Approach 1: Source Text (Current — Improve It)

Keep generating source text but make it more robust.

### Improvements

**Use `quote!`-style structured generation instead of string concatenation:**

The consumer (or library) could use a small builder API:

```rust
let mut stubs = StubBuilder::new();
stubs.add_struct("Point", &[], &[
    ("x", FieldType::I32),
    ("y", FieldType::I32),
]);
stubs.add_extern_fn(
    "__toylang_impl_make_vec",
    &[Param::phantom(), Param::phantom()],
    ReturnType::Generic("Vec", "Point"),
);
let source: String = stubs.to_rust_source();
```

This would:
- Prevent malformed source (missing commas, unbalanced braces)
- Keep the source text approach's resilience to nightly changes
- Make the library responsible for generating syntactically valid stubs
- The consumer just describes *what* to generate, not *how*

**The library could own this entirely in Options C/E**, since `LangDef` already contains all the information needed to generate stubs. The consumer wouldn't need a `generate_stubs` method at all — the library would have a `LangDef::to_rust_source()` that does it.

### Limitations

Still can't easily express:
- Trait implementations (e.g., `impl Hash for Point`)
- Complex generic bounds (e.g., `where T: Clone + Debug`)
- Associated types
- Enum definitions with variants

But for the current milestone (structs + extern functions), source text works well.

---

## Approach 2: Programmatic HIR Construction

Instead of generating source text, construct HIR nodes directly using rustc's internal APIs.

### How it would work

Override the `hir_crate` query or hook into an early compilation pass to inject synthetic `Item` nodes:

```rust
// Pseudocode — actual rustc HIR API is more verbose
let struct_item = hir::Item {
    ident: Ident::from_str("Point"),
    kind: hir::ItemKind::Struct(
        hir::VariantData::Struct {
            fields: vec![
                hir::FieldDef { ident: "x", ty: hir_ty_i32 },
                hir::FieldDef { ident: "y", ty: hir_ty_i32 },
            ],
        },
        generics,
    ),
    // ...
};
```

### Why this is hard

1. **DefId allocation.** DefIds are assigned during parsing via `Resolver`. Injecting items after parsing requires manually allocating DefIds, which means poking at `Definitions` tables that aren't designed for external manipulation.

2. **Name resolution.** rustc's `Resolver` builds name resolution tables during parsing. Injecting items after this pass means names won't resolve — code that says `Point` won't find the injected struct unless you also patch the resolution tables.

3. **HIR API churn.** HIR node constructors change frequently between nightlies. `ItemKind::Struct` takes different arguments on different versions. Source text generation avoids this because the *syntax* is stable even when the AST isn't.

4. **No validation.** If you construct an invalid HIR node (wrong field count, missing span, inconsistent generics), you get an ICE deep in rustc rather than a clear parse error.

5. **Arena allocation.** HIR nodes live in rustc's arena allocator (`tcx.arena`). You need a valid arena reference to allocate nodes, and you need to do it at the right compilation phase.

### When it might make sense

- If you need to define types that can't be expressed as Rust source (e.g., types with custom calling conventions, types with layout attributes that don't exist in Rust syntax)
- If you need to avoid the overhead of parsing (unlikely to matter — parsing is fast)
- Cross-crate scenarios where you need types in `.rmeta` metadata

### Assessment

**Not recommended.** The cost (fragility, complexity, nightly churn) vastly outweighs the benefit. Source text generation does everything the project needs.

---

## Approach 3: Metadata Injection (.rmeta)

Inject type definitions into rustc's `.rmeta` metadata format so they appear as if they came from a pre-compiled crate.

### How it would work

1. Build a synthetic `.rmeta` file containing type definitions
2. Feed it to rustc as an `--extern` dependency
3. rustc loads the metadata and makes the types available for name resolution

### Why this is hard

- `.rmeta` is an internal binary format with no stability guarantees
- It changes between nightly versions (sometimes between point releases)
- The `rustc_metadata` crate that reads/writes it is complex (~20k lines)
- You'd need to serialize `AdtDef`, `FieldDef`, `GenericPredicates`, etc. in exactly the format rustc expects
- Any mismatch causes `DecodeError` panics or silent corruption

### When it might make sense

This is the right approach for **cross-crate** scenarios: when crate A defines a type in the consumer's language, and crate B (compiled separately) needs to use that type. The architecture guide lists this as a "known unknown."

### Assessment

**Not needed now.** The project compiles everything in a single rustc session. Cross-crate support is a future milestone. When it's needed, this should be revisited — but probably by hooking into the metadata serialization pass rather than building `.rmeta` files from scratch.

---

## Approach 4: Override `type_of`/`adt_def` Queries

Instead of defining struct types via source stubs, override the queries that provide type information:

```rust
providers.type_of = |tcx, def_id| {
    if is_my_type(def_id) {
        return my_type_definition(tcx, def_id);
    }
    default_type_of(tcx, def_id)
};
```

### Why this doesn't work

The problem is **DefId creation**. Before you can override `type_of(def_id)`, the DefId must exist. DefIds are created during parsing. If you skip parsing, there's no DefId to query. You'd need to:

1. Allocate DefIds manually (poking at `Definitions`)
2. Register them in the resolver (so name resolution works)
3. Override `type_of`, `generics_of`, `predicates_of`, `adt_def`, etc.
4. Also override `hir_owner` (so queries that inspect HIR don't ICE)

This is essentially reimplementing rustc's frontend for your types, which is far more work than generating source text that lets rustc's existing frontend handle it.

### Assessment

**Not viable.** The source text approach works because it piggybacks on rustc's existing parsing + name resolution + DefId allocation. Bypassing that pipeline requires reimplementing too much of it.

---

## Approach 5: Use a Real .rs File on Disk

Instead of a virtual FileLoader, write the generated source to an actual file.

### How it would work

```rust
// Write stubs to a temp file
let stub_path = out_dir.join("__toylang_stubs.rs");
std::fs::write(&stub_path, generated_source)?;

// The test/app file includes it:
// mod __toylang_stubs;  // or
// include!("path/to/__toylang_stubs.rs");
```

### Trade-offs vs FileLoader

**Pros:**
- Simpler — no custom FileLoader needed
- Debuggable — you can `cat` the file to see what was generated
- Works with standard `mod` declarations if the file is in the right directory
- Works with `include!()` if you want an explicit path

**Cons:**
- Temp file management (cleanup, unique names, permissions)
- The test/app file needs to know the path or the file needs to be in a specific location
- Slightly slower (disk I/O, though negligible)
- The generated file might be checked into version control accidentally

### Assessment

**Viable alternative.** Simpler than the FileLoader approach. The FileLoader is more elegant (no temp files) but a real file on disk is equally correct and easier to debug. Could be offered as an option alongside the FileLoader.

---

## Approach 6: `#[cfg]`-gated Stub File Checked Into Source

Instead of generating stubs at compile time, maintain a stub file as part of the project:

```rust
// src/__toylang_stubs.rs (checked in)
pub struct Point {
    pub x: i32,
    pub y: i32,
}
extern "C" {
    pub fn __toylang_impl_make_vec(...) -> Vec<Point>;
}
```

### Assessment

**Not suitable for a library.** Works for a single project but doesn't scale — every time the consumer's types change, the stub file must be manually updated. The whole point of stub generation is that it's automatic.

---

## Recommendation for the Library Split

### For Options A-D (consumer needs `tcx`)

The library should provide a **`StubBuilder`** that generates source text from `LangDef`:

```rust
/// Generates syntactically valid Rust source from type/function definitions.
/// The library owns this so the consumer doesn't need to worry about
/// Rust syntax details, phantom parameter counting, or extern block formatting.
pub struct StubBuilder { ... }

impl StubBuilder {
    pub fn from_lang_def(defs: &LangDef) -> Self { ... }

    /// Add additional custom Rust source (e.g., trait impls, use statements).
    /// Appended verbatim after the generated struct and extern declarations.
    pub fn add_custom_source(&mut self, source: &str) { ... }

    /// Produce the final Rust source string.
    pub fn to_source(&self) -> String { ... }
}
```

In Option C, the `LangCodegen::generate_stubs()` method could have a **default implementation** that calls `StubBuilder::from_lang_def(defs).to_source()`. Consumers only override it if they need custom stubs (e.g., trait impls, `#[repr(C)]`, etc.).

### For Option E (consumer doesn't need `tcx`)

The library generates stubs internally from `LangConfig` — the consumer never sees source text at all.

### Phantom parameters

The phantom `_dep` parameter count is currently computed by scanning the consumer's AST for Vec operations. In the library, this should come from the `rust_deps` information (Options C/E) or from `fn_rust_deps()` (Options A/B). The library knows how many Rust deps a function has and can generate the right number of phantom parameters automatically. The consumer should never need to count these.

### `#[repr(C)]` question

The stubs currently use default Rust repr. This is correct because the `layout_of` override replaces rustc's layout entirely. But it would be **safer** to add `#[repr(C)]` to generated structs, ensuring that if the layout_of override is ever bypassed (e.g., by a rustc optimization that caches layouts before the override fires), the field order still matches expectations. This is a one-line change in the `StubBuilder`.

---

## Summary Table

| Approach | Complexity | Nightly resilience | Expressiveness | Recommended? |
|----------|-----------|-------------------|----------------|-------------|
| **1. Source text (improved)** | Low | High | Medium | Yes |
| **2. HIR construction** | Very high | Low | High | No |
| **3. .rmeta injection** | Very high | Very low | High | No (future) |
| **4. Query overrides for types** | High | Medium | Medium | No |
| **5. Real file on disk** | Low | High | Medium | Maybe |
| **6. Checked-in stubs** | Lowest | High | Low | No |

**Source text generation (Approach 1) is the right choice.** The improvements (StubBuilder, library-owned generation, `#[repr(C)]`) address its weaknesses without adding complexity. The other approaches solve problems that don't exist yet (cross-crate, non-Rust-expressible types) at high cost.
