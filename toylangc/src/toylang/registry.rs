use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use crate::toylang::typed_ast::ResolvedType;

/// A Toylang struct field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyField {
    pub name: String,
    pub rust_type: ResolvedType,
}

/// A Toylang struct definition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyStruct {
    pub type_params: Vec<String>,   // e.g. ["A", "B"]; empty for non-generic
    pub fields: Vec<ToyField>,
}

/// All Toylang definitions visible to the current compilation.
///
/// `structs` and `functions` use `BTreeMap` rather than `HashMap` so iteration
/// is deterministic — load-bearing for sidecar byte-equality
/// (`docs/architecture/sidecar-format.md` "Determinism requirements"). The
/// `serialize_sidecar` / `deserialize_sidecar` machinery in
/// `crate::sidecar` round-trips this whole struct.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToylangRegistry {
    pub structs: BTreeMap<String, ToyStruct>,
    pub functions: BTreeMap<String, ToyFunction>,
    /// Rust `use` imports (e.g. "std::alloc::Global"). Emitted as `pub use` in stubs.
    pub imports: Vec<String>,
    /// Phase 2 C: toylang `impl rust_trait for toylang_type` blocks. Each entry
    /// is one source-level impl block; the methods inside are stored as
    /// `ToyFunction`s with the implicit `self` parameter elevated to an
    /// explicit `&ToyStruct` first parameter (architecture §6.2; Case 4).
    pub trait_impls: Vec<ToyImpl>,
    /// Phase E Path 2 — content-addressed typeids for Sky structs
    /// (architecture §10.6 / §10.8). Each entry maps a stable `u64` typeid
    /// (computed via `crate::typeid::compute(name, &[])` over a Sky struct's
    /// qualified identity) to the source-level `(name, type_args)` pair that
    /// produced it. The decoding side — the `layout_of` override fired on
    /// `__ToylangOpaque<HASH>` — uses this table to recover the Sky type and
    /// dispatch to the existing size/align computation. Populated by
    /// `populate_typeid_table` after the typing pass finishes, before the
    /// sidecar is written; serialized so downstream compiles can decode
    /// upstream typeids that originated in a previously-compiled Sky library.
    ///
    /// `BTreeMap` for sidecar byte-equality (same rationale as `structs` /
    /// `functions` above). `#[serde(default)]` because pre-Path-2 sidecars
    /// don't carry the field; loading one yields an empty table, which is
    /// harmless until Phase 3 starts referencing typeids that would need it.
    #[serde(default)]
    pub typeid_table: BTreeMap<u64, (String, Vec<ResolvedType>)>,
}

impl ToylangRegistry {
    /// Phase E Path 2 / Phase 1.3 — populate the typeid table by hashing
    /// every Sky struct in `structs`. Idempotent: calling repeatedly produces
    /// the same table.
    ///
    /// The mapping is `compute(name, &[]) → (name.clone(), vec![])` per
    /// architecture §10.4.5 Path 2's "per-struct identity, not
    /// per-instantiation" interpretation — `Wrapper<i32>` and `Wrapper<i64>`
    /// share `HASH_FOR_WRAPPER` and disambiguate at the type level via their
    /// own generic args slot. Per-instantiation typeids are reserved for
    /// non-export and comptime-produced types (§10.7 Cases 2 + 3), out of
    /// scope for this phase.
    pub fn populate_typeid_table(&mut self) {
        self.typeid_table.clear();
        for name in self.structs.keys() {
            let typeid = crate::typeid::compute(name, &[]);
            self.typeid_table.insert(typeid, (name.clone(), Vec::new()));
        }
    }
}

/// Phase 2 C: a toylang `impl <RustTrait> for <ToyStruct> { fn … }` block.
/// `trait_name` is the short name of the Rust trait (e.g. "Clone"); it must
/// be `use`-imported elsewhere in the source so the oracle can resolve its
/// DefId. `self_type_name` is the toylang struct the impl is for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyImpl {
    pub trait_name: String,
    pub self_type_name: String,
    pub methods: Vec<ToyImplMethod>,
    /// Session 10 — Sky architecture §9. Source-level `export impl Trait for
    /// Type { ... }`. Currently REQUIRED to be true for any Rust caller to
    /// dispatch through the impl (rustc needs a DefId for the impl methods).
    /// Sky's locked design likewise treats impl blocks as inherently boundary-
    /// crossing items.
    #[serde(default)]
    pub is_export: bool,
}

/// A method inside a `ToyImpl`. Stored as `ToyFunction` plus the method's
/// source-level name; the `params` of the inner function include `self` as a
/// `&ToyStruct` first parameter (synthesized by the parser from the
/// `&self` token).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyImplMethod {
    pub name: String,
    pub func: ToyFunction,
}

/// A parsed parameter in a Toylang function signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyParam {
    pub name: String,
    pub ty: ResolvedType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToyFunction {
    pub type_params: Vec<String>,   // e.g. ["T"]; empty for non-generic functions
    pub params: Vec<ToyParam>,
    pub return_ty: Option<ResolvedType>,
    pub body: Option<crate::toylang::ast::Block>,
    /// Session 10 — Sky architecture §9. True iff the source declared
    /// `export fn …`. Non-export body-bearing fns get NO `pub fn` shell in
    /// the stub rlib (architectural commitment that rustc cannot name them).
    /// `main` is implicitly exported because the Rust shim references
    /// `__toylang_main`. Body-less (extern) declarations are always emitted
    /// regardless of this flag — they declare Rust functions, not Sky items.
    #[serde(default)]
    pub is_export: bool,
}

impl ToyFunction {
    /// True iff this function takes Sky-side type parameters that haven't
    /// been substituted yet. Gates the eager-typecheck and registry-walk
    /// passes — abstract-arg fns can only be processed once concrete args
    /// arrive (via the per-Instance substituted pass for typecheck, and
    /// via the CGU walk or transitive walk for codegen).
    ///
    /// CLAUDE.md compiler law: non-generic is the degenerate case of generic.
    /// This helper expresses the architectural intent ("skip items whose args
    /// are still abstract") rather than the degenerate-case shortcut.
    pub fn has_abstract_args(&self) -> bool {
        !self.type_params.is_empty()
    }
}
