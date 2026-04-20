# Architecture Decisions

Why each major architectural choice in erw was made. Pulled out of the main architecture guide so that "what the code is" (`rust-interop-guide.md`) stays separate from "why the code is that way" per `docs/meta.md`'s category split.

Each section covers one decision with the alternatives considered. Deeper reasoning docs exist for the biggest choices — those are linked rather than duplicated.

---

## Why deep monomorphization walk

Previously, `collect_toylang_fn_deps` reported toylang callees to rustc, causing rustc to process internal functions through the (now-retired) `per_instance_mir` query / `symbol_name`. The deep walk eliminates this: the two walkers (`collect_rust_deps_recursive` and `walk_and_stash_internal_callees`, `rust-interop-guide.md` §4.4) cooperatively handle Rust-dep discovery and internal-callee stashing without exposing internal callees to rustc. Internal functions live in `ToylangState.toylang_instances` and get codegenned directly; `walked_entry_points` keeps each internal function body walked-and-stashed exactly once per compilation.

The alternative (letting rustc see everything) mixes concerns: rustc's emission decisions for "purely internal" consumer fns become a constraint we have to work around, and we pay for per-Instance query-provider dispatch for items we're going to codegen ourselves anyway.

## Why split globals (immutable OnceLock + mutable Mutex)

Rustc's query providers fire on Rayon worker threads. The original design used a single `Mutex<FacadeGlobals>` to serialize all consumer code. This worked until Phase 4's `stdout()` test, which triggered a deadlock: `call_generate_and_compile` held the mutex while consumer codegen ran; codegen called `tcx.symbol_name(stdout)`; our `lang_symbol_name` provider tried to call `default_symbol_name()` which tried to re-lock the same mutex.

The fix splits state by mutability: immutable config (callbacks, vtable, default providers) goes in `OnceLock` statics (no locking needed for reads); mutable state (consumer_state, lang_obj_path) stays behind a Mutex. Query providers reading only config are lock-free, so they can execute during `generate_and_compile` without deadlock. See `docs/arcana/GenerateCompileMutexLock-GCMLZ.md` (`@GCMLZ`).

The Mutex on `consumer_state` still serializes all callbacks that need `&mut` access, preserving the single-threaded execution guarantee for toylang code.

## Why ABI-coerced return types for Rust function declarations

When declaring an LLVM function that will be called as a Rust function, the return type must match rustc's ABI coercion, not toylang's representation. For an 8-byte struct like `Stdout`, rustc returns `i64` (Direct scalar in register `x0`), but toylang's `resolved_to_inkwell` produces `[8 x i8]` (LLVM aggregate in memory). LLVM uses different code paths for the two — declaring the wrong one produces garbage return values.

Phase 4 fixed this in all three Rust call paths (MethodCall, StaticCall, FnCall use-import) by using `parse_coerced_type(coerced_ret)` for the declaration. When the ABI type differs from the toylang type, codegen stores the return value through an alloca to reinterpret the bits (type-punning bitcast via memory). See `docs/arcana/ABICoercedReturnTypeInFunctionDeclarations-ACRTFDZ.md` (`@ACRTFDZ`).

Internal toylang-to-toylang calls still use `resolved_to_inkwell` because their ABI is fully owned by toylang — no rustc coercion applies.

## Why consumer state is `dyn Any` in the facade

The facade is generic over `C: LangCallbacks` but can't store `dyn LangCallbacks` (the `'tcx` lifetime on methods breaks object safety). Consumer state is stored as `Box<dyn Any + Send + Sync>` and passed to callbacks as `&mut dyn Any`. The consumer downcasts to its concrete type. This keeps the facade library-agnostic.

## Why `is_consumer_type` / `is_consumer_fn` are callbacks

Originally the facade copied consumer name sets into static `HashSet` globals. Now these are vtable callbacks — the facade asks the consumer directly "is this yours?" via `is_consumer_type` / `is_consumer_fn`. No duplicated state.

## Why opaque stubs with 0-field layout

Reporting real field counts in `FieldsShape` caused ABI code to index into the ADT's stub fields (which are dummy types). With 0 fields, the ABI code treats consumer types as opaque memory blobs.

## Why the facade interleaves with rustc's monomorphization phase

The facade's query providers hook into rustc during monomorphization rather than as a pre-pass (e.g., `Callbacks::after_analysis`) or a post-pass (e.g., a `CodegenBackend` plugin receiving CGUs). This is not a stylistic choice — it's the only phase where the handoff the facade needs to perform is actually possible.

Short form: rustc's monomorphization collector is the only entity that walks both Rust and (via the facade) consumer source. Letting the collector drive discovery lets the facade **tell rustc the leaves** — the concrete type-argument tuples for every Rust item called directly from consumer code — and rustc walks the transitive closure from there (trait resolution, associated types, nested generics, drop glue). The alternative — the facade reimplementing rustc's trait/generic resolution machinery — is tens of thousands of lines of rustc internals reimplemented. The interleaving is how the facade *avoids* that reimplementation.

**For the full argument** with a seven-case taxonomy of consumer architectures (which cases pre-pass can handle, which force interleaving, and why) — including complete code examples and the generic-method-on-generic-trait case that kills the last over-approximation workaround — see `docs/reasoning/why-interleaved-monomorphization.md`.

## Why override `optimized_mir` instead of hooking `mir_built`

`mir_built` fires once per function DEFINITION, not per instantiation. For generic functions, rustc calls `mir_built` once for the generic definition and substitutes internally. `optimized_mir` ALSO fires once per DefId — but rustc's mono collector walks its returned body for each caller Instance, substituting Params per caller during the walk (the same substitution engine that handles every generic Rust function). That per-caller substitution is what makes the DefId-keyed override sufficient for dep discovery.

**Stage-3 migration note:** erw previously shipped a custom Instance-keyed `per_instance_mir` query via a 4-patch rustc fork. Stage 3 retired that query in favor of the sanctioned `Config::override_queries` path on `optimized_mir` — same dep-discovery behavior, three fewer fork patches, no custom query plumbing. The insight that made the migration viable is the asymmetry between Rust-dep output (Params fine, rustc substitutes) and internal-callee output (must be concrete, no downstream substitutor exists for toylang LLVM IR). See `docs/reasoning/dep-discovery-approaches.md` for the full comparison and `docs/reasoning/rustc-fork-design-space.md` §4.1 for the fork-reduction accounting.

**Why the Param-bearing output is bounded:** the natural follow-up worry about Approach B is whether the returned `(DefId, GenericArgsRef)` pairs can grow unboundedly complex in deep call trees with intricate substitution chains. They can't — source-level scoping guarantees every dep-arg expression is closed over {outer_fn_params ∪ concrete_rust_types}, and this property survives inference + lowering for any conventional type system. `docs/reasoning/why-outer-params-suffice.md` walks the structural argument and enumerates the apparent breakers (closures, `impl Trait`, inferred args) showing each lowers away before dep discovery runs.

## Why explicit type args instead of inference

Type inference was attempted but caused cascading problems (backward propagation, fragile heuristics for Vec element types). Explicit type args eliminated ~150 lines of inference machinery.

## Why no mir_built or borrowck overrides

Consumer functions have `unreachable!()` bodies — valid Rust that passes all checks normally. No need to intercept.

---

## See also

- `docs/architecture/rust-interop-guide.md` — the architecture these decisions produced.
- `docs/reasoning/why-interleaved-monomorphization.md` — the foundational seven-case taxonomy for "why the facade exists at all."
- `docs/reasoning/rustc-fork-design-space.md` — fork-reduction design space, the road to stage-4's zero-fork landing.
- `docs/reasoning/dep-discovery-approaches.md` — Approach A vs B comparison; the asymmetry insight behind stage 3.
- `docs/reasoning/why-outer-params-suffice.md` — why Approach B's Param-bearing dep args are always expressible over the outer fn's generic scope; refutes the "unbounded symbolic complexity" worry.
- `docs/reasoning/trait-call-investigation.md` — trait method dispatch investigation.
- `docs/arcana/` — cross-cutting invariants these decisions produced (`@GCMLZ`, `@ACRTFDZ`, etc.).
