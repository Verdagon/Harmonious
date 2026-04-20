# Rust-Dep Args Are Always Expressible Over the Outer Fn's Params

A follow-on to `dep-discovery-approaches.md`. That doc establishes that both the retired Approach A (`per_instance_mir`, Instance-keyed, concrete args) and the shipping Approach B (`optimized_mir` override, DefId-keyed, Param-bearing args) correctly produce the full per-Instance Rust-dep set. This doc answers a deeper follow-up worry about Approach B specifically: **when the walker returns Param-bearing `(DefId, GenericArgsRef)` pairs, can those args always be expressed as closed expressions over the outer consumer fn's generic parameters (plus concrete Rust types), regardless of how deep the call tree or how complex the intermediate substitutions?**

**TL;DR:** yes, and it's not a toylang-specific property. It follows from any well-typed AST's structure: every call site's type args are, by scoping, closed over {outer_fn_params ∪ concrete_types}. Inference preserves this; lowering preserves this; there is no feature in Rust (or any conventional type system) whose surface survives front-end passes and introduces a dep-arg slot that can't be spelled this way. The explicit-type-args rule in current toylang is front-end scaffolding, not a load-bearing invariant.

---

## The worry

Under Approach B, `collect_generic_rust_deps(def_id) → Vec<(DefId, GenericArgsRef<'tcx>)>` returns args that may carry `ty::TyKind::Param` placeholders bound to the outer fn's generic parameters. Rustc's mono collector substitutes those Params per caller during its walk of our synthesized body.

The intuition concern: imagine a consumer call tree

```toylang
fn a<T>() { b<Vec<T>>() }
fn b<U>() { c<Vec<U>>() }
// ... 20 levels of wrapping ...
fn z<W>() { leaf<HashMap<W, Box<W>>>() }
fn leaf<X>() { Rust::thing<X>() }
```

The walker substitutes per level (`resolve_toylang_callee`), so by the time it reaches `leaf` it has built a deeply nested `ResolvedType` for X. The leaf dep carries `(Rust::thing_DefId, [<20-deep Vec-and-Box tree with Param(T) at the leaf>])`. Can that really go back to rustc as a `GenericArgsRef` and work? And is there ever a case where the walker needs to produce something that *isn't* expressible this way — e.g., a dep arg that references a type variable not bound by `a<T>`'s scope?

---

## The invariant, stated precisely

**For any consumer function `f<T_1, ..., T_n>`, the dep list `collect_generic_rust_deps(f_def_id)` returns only `GenericArgsRef` entries whose type slots are expressions over the set `{T_1, ..., T_n} ∪ concrete_rust_types`.**

"Concrete rust types" here means types whose construction doesn't require a Param — `i32`, `Vec<i32>`, user-defined Rust structs, etc. The set is closed under composition (`Vec<_>`, `Box<_>`, struct instantiation, trait projections, etc.).

A Param referring to a *nested consumer callee's own* type parameter never escapes into a returned dep, because the walker substitutes each nested callee's params at its call site (`resolve_toylang_callee` in `callbacks_impl.rs`). Substitutions are chained back through the tree to `f`'s scope.

---

## Why it holds: the structural argument

The invariant is a consequence of source-level scoping in any well-typed AST. At any call site in `f`'s transitive consumer call graph, the type args written at that site can only name types that are in scope at that site. The in-scope type set at any depth is:

1. The enclosing fn's type params (which, via substitution chain back to `f`, reduce to `f`'s params).
2. Concrete Rust types named in the source.
3. Types built compositionally from (1) and (2) — `Vec<T>`, `HashMap<A, B>`, `&T`, etc.

There is no fourth source. A free type variable at a call site would be a type-checking error (unbound type parameter) — the AST wouldn't exist.

When the walker recurses into a nested callee, it substitutes that callee's type parameters with the call-site args. The substituted args are themselves closed over `f`'s scope (by induction over the recursion), so the substituted body's call sites also have args closed over `f`'s scope. Fixpoint: every leaf Rust-dep arg is closed over `f`'s scope.

---

## Apparent breakers, and why each lowers away

A well-typed AST here means "the output of a front-end that has done typecheck + inference + lowering." Each of the following looks like it might introduce a non-scoped type, but a real front-end lowers them before dep discovery runs.

### Inferred type args

`Vec::new()` with no turbofish, type inferred from context. Inference is an earlier pass; by the time dep discovery runs, the typed AST has every call site's args resolved to closed expressions over outer params + concrete types. That's inference's job: resolve unification variables to closed expressions. Toylang's current "explicit args required" policy is scaffolding to avoid implementing inference in the proof-of-concept — it doesn't protect any invariant that inference couldn't also uphold.

### Closures / anonymous function types

A closure `|x| x + 1` has a compiler-synthesized anonymous type. But closures lower to structs-with-trait-impls (what rustc does internally at HIR→MIR, what languages like Java and C# also do). A front-end can desugar `|x| x + 1` into a named struct `__closure_4` implementing `Fn(i32) -> i32`. After lowering, the "closure" is just a regular consumer struct; its DefId is a normal named item; its use sites reference it by name. No anonymous DefIds reach dep discovery.

### `impl Trait` in return position / TAIT

`fn foo<T>() -> impl Iterator<Item=T> { ... }` looks like it introduces an opaque type with a fresh DefId. Lowering: the front-end picks a concrete return type (it has to — there's only one) and rewrites the return annotation. If the language wants to preserve the hiding for the caller, the front-end generates a named nominal type and returns that explicitly. Either way, after lowering the return type is named.

### Async / generators

State-machine transforms. Rustc already lowers these to structs implementing `Future` / `Iterator`. Same pattern: lowering produces named items.

### Inferred const-generic values

`[T; _]` with `N` inferred. Const inference runs in the same typecheck pass as type inference; by dep discovery time every const slot has a fixed value (literal, evaluated expression, or closed `ConstKind` over outer const params). Not a breaker for the same reason as type inference.

### Higher-ranked *type* parameters

`for<T> fn(T) -> T` as a first-class value. Would require emitting a `BoundTy` slot with a fresh binder index not bound by an outer param. Rust doesn't have this (only HRTB *lifetimes* exist, and we erase those to `re_erased` per `@ELASZ`). Adding it to any language requires unboxed higher-rank polymorphism, which nobody ships.

### Reflection / `typeof` / runtime type synthesis

Constructs that produce types from expression inference rather than syntax. Doesn't exist in Rust or in any language toylang might want to resemble.

---

## Operational concerns remain

Expressibility is not the same as cost. Two operational concerns about Approach B are real and orthogonal to this invariant:

- **Per-entry-point subtree redundancy.** If entries `e1<T>, e2<T>, e3<T>` all call the same deep helper `shared<T>()`, `optimized_mir(e1|e2|e3)` each re-walks `shared`'s subtree. Solvable with walker memoization keyed by `(def_id, callee-args)`; not a correctness issue.
- **Rustc substitution-engine stability across versions.** The shipping architecture depends on rustc's collector correctly substituting Params in override-returned bodies. POC #1 verified the mechanism; `TypingEnv::fully_monomorphized()` tolerates Param-bearing sigs (the name declares a typing *mode*, not an input assertion). Risk is localized to a well-documented surprise; no invariant breakage across rustc versions has been observed.

Neither is a reason to reconsider Approach A — Approach A had worse versions of both (walker shadowing rustc's substitution engine; per-Instance walks multiplying redundancy).

---

## The upshot

Approach B's Param-bearing output is bounded in expressibility by what the source program spells (or what inference resolves it to, which is the same set). Expression complexity is a function of source program complexity — rustc's substitution engine handles any such complexity by definition, because it's the same machinery every generic Rust fn uses.

If this invariant ever breaks, it will be because toylang has gained a feature whose lowering introduces dep-arg slots outside the scoping rule — and no such feature exists in any conventional type system. Adding one would require rethinking monomorphization at a level that affects Approach A equally.

---

## See also

- `docs/reasoning/dep-discovery-approaches.md` — the foundational A-vs-B comparison; this doc deepens the expressibility claim made there in passing.
- `docs/reasoning/why-interleaved-monomorphization.md` — why dep discovery must plug into rustc's monomorphization phase at all; the precondition that makes both approaches possible.
- `docs/reasoning/architecture-decisions.md` — "Why override `optimized_mir` instead of hooking `mir_built`" covers the per-DefId / per-caller-substitution split that this doc's invariant relies on.
- `docs/architecture/rust-interop-guide.md` §2.2 — the shipping override's implementation.
- `docs/arcana/EarlyBoundLifetimeArgsSynthesized-ELASZ.md` (`@ELASZ`) — the lifetime-erasure rule that keeps HRTB lifetimes out of the expressibility picture.
