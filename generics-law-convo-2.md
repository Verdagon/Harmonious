# Generics-law conversation 2

First "deep audit" ask.

---

## 👤 User

can you do a very deep audit, and dispatch many agents, to find everywhere where we're special-casing non-generic functions, and find all the places where we're not abiding by the principle "non-generic functions are just a normal case of generic functions, and should cleanly come from the design of generic functions"? find everywhere where something wouldn't work for a generic function please

---

## 🤖 Claude

This is a real audit-shaped question. Dispatching 3 parallel agents on different angles of the codebase.

---

## 🤖 Claude

Three agents back with a comprehensive picture. Honest synthesis below. The compiler-law principle is **partially violated** in real, named ways — the fence catches the explicit `is_empty()` branches but misses the subtler patterns. Findings clustered by severity:

## A. Architectural — broken (or silently broken) for generic items today

| # | Site | Problem | Severity |
|---|---|---|---|
| **A1** | `toylangc/src/toylang/registry.rs:147-158` (`ToyImpl` schema) | `ToyImpl` has no `type_params` field at all. The data model assumes zero impl-block generics. | **Architectural** |
| **A2** | `toylangc/src/stub_gen.rs:352-388` (trait-impl emission) | Emits `impl #trait_ident for #self_ident { ... }` with no impl-generics clause and no type-args on the self type. `generics_for_impl_block` is called for the *inherent*-impl path (line 220) but NOT for the trait-impl path. Would emit `impl Clone for Wrapper` instead of `impl<T> Clone for Wrapper<T>` — rustc rejects. | **Architectural** |
| **A3** | `toylangc/src/toylang/callbacks_impl.rs:455-468` (trait-impl populate) | Hardcodes `Instance::new_raw(def_id, GenericArgs::empty())` for trait-impl methods. Fallback symbol `__toylang_internal__<Self>__<Trait>__<method>` carries NO type-arg suffix, so `<Wrapper<i32> as Clone>::clone` and `<Wrapper<i64> as Clone>::clone` collide on the same symbol. | **Latent collision** |
| **A4** | `toylangc/src/toylang/callbacks_impl.rs:501-538` (accessor populate) | Generic-struct accessors filtered out via `has_abstract_args()`. The architecture intended Channel A (CGU walk) to catch them, but `llvm_gen.rs:2042` looks up the accessor name in `registry.functions` — and accessors live in `accessor_pairs`, not `functions`. The lookup fails silently. End-to-end: `wrapper.<i32>.inner` for `Wrapper<T>` has **no codegen path at all**. | **Architectural** — silent drop |
| **A5** | `toylangc/src/typeid.rs:29-50` + every call site (`stub_gen.rs:170`, `registry.rs:85-91`) | `typeid::compute(name, &[])` everywhere. `typeid_table.value.1: Vec<ResolvedType>` is always empty. Function *supports* per-instantiation hashing (its own tests prove it) but no call site uses it. `Wrapper<i32>` and `Wrapper<i64>` get the same typeid. Currently safe because layout doesn't key off typeid alone, but the doc at `typeid.rs:8` *claims* the typeid is the key by which `queries/layout.rs` recovers `(name, type_args)` — and it cannot. | **Architectural assumption baked in** |
| **A6** | `rustc-lang-facade/src/lib.rs:413-465` (`SkyUniverse.struct_infos`) | Keyed by struct *name only* (`HashMap<String, Arc<dyn Any>>`). Two Sky libs with same struct name would silently overwrite. Currently safe only because `monomorphize_type` re-substitutes from rustc's `Ty<'tcx>.args` per call. | **Latent collision** |

## B. Latent — works today, breaks with one more refactor

| # | Site | Problem |
|---|---|---|
| **B1** | `rustc-lang-facade/src/queries/layout.rs:151-178` | Branches on `source_field_count` ∈ {0, 1, 2} where 1 = non-generic stub shape and 2 = generic stub shape (wrapper + PhantomData). 3+ panics. Tight coupling to `stub_gen.rs:177-188`'s two shapes. |
| **B2** | `toylangc/src/toylang/callbacks_impl.rs:1432-1440` (`walk_and_stash_internal_callees`) | Pushes `ToylangInstance { stub_def_id: None, instance: None }` for transitive callees. Items reached transitively never qualify for extern-wrapper codegen. For a generic helper reachable both transitively AND from a Rust caller, two codegen entries with different symbols. |
| **B3** | `toylangc/src/llvm_gen.rs:34-47` vs `callbacks_impl.rs:1218-1227` | Two divergent internal-symbol mangling paths (`internal_symbol_for_instance` walks `instance.args`; `compute_internal_symbol_from_type_args` walks Sky-side `ResolvedType` args). For trait-impl methods reached via transitive walk, the Sky-side path produces a free-fn-shaped symbol that won't match the extern wrapper's. |
| **B4** | `rustc-lang-facade/src/lib.rs:656-676` (`is_consumer_trait_impl_method`) | Uses `instantiate_identity()` and discards substs. Downstream `symbol_name.rs:75` builds `__impl_method__<Self>__<Trait>__<method>` callback name with no type-arg distinction. Today safe because Path B routes through `default_symbol_name` (which IS args-correct), but the callback-name machinery is structurally lossy. |
| **B5** | `toylangc/src/oracle.rs:208-217` (`find_inherent_method`) + `oracle.rs:693-724` (`find_trait_impl_method_def_id`) | Both match purely on ADT name; first-match-wins. `impl Wrapper<i32>` and `impl<T> Wrapper<T>` would both match a query for `Wrapper`. |
| **B6** | `toylangc/src/oracle.rs:282-297` (`try_resolved_to_rustc_ty`) | Hard panic if `TypeParam` reaches the converter. Invariant ("Approach A must have substituted") is comment-only — no automated check. |
| **B7** | `toylangc/src/oracle.rs:746-783` (`build_generic_args_for_item`) | @ETASTZ-tagged: extra `Type` args silently dropped. Already in tech-debt. |

## C. Coverage gaps — code that looks right but no fixture exercises it

| # | Gap | Caught by current 266 tests? |
|---|---|---|
| **C1** | **No fixture has a generic impl block.** case4's `impl Clone for Widget` and case6_lib's `impl Clone for Box` are both non-generic. Findings A1–A3 are entirely uncovered. | **No** |
| **C2** | **No fixture instantiates the same generic struct at two distinct types** in one crate. Would expose A5/A6 if anything ever keyed metadata by typeid alone. | **No** |
| **C3** | **No fixture uses a Sky generic struct as the T argument of another Sky generic struct** (`Wrapper<Box<i32>>` where both are Sky). Stresses recursive substitution. | **No** |
| **C4** | **No fixture uses generic-struct field access from Rust caller** (the trigger for A4). Would expose the silent codegen drop. | **No** |
| **C5** | **No fixture has trait methods with method-level type params** (e.g., `fn serialize<W: Write>(&self, w: &mut W)`). Architecture §2.6 calls this case out as the load-bearing pattern that kills the over-approximation workaround. | **No** |

## D. Already-fenced degenerate fast paths (verified, OK)

- `monomorphize_type` (`callbacks_impl.rs:968-981`) — `if !type_params.is_empty()` fast-path. Fence-allowed; semantically equivalent to the general path.
- `stub_gen.rs:176-188` struct-shape branch — fence-allowed `phantomdata-only-when-generics-present`. Forced by rustc's E0392.
- `resolved_type_to_syn` (`stub_gen.rs:94`) — skips `<>` when empty (rustc rejects `Foo<>`).
- `oracle.rs:397` `resolved_type_to_mangled_name` skip — verified degenerate.
- `typeid::compute(name, &[])` — intentional per §10.4.5 ("per-instantiation typeids out of scope for Phase 2").

## Honest framing

**The big load-bearing gap is A1–A3 + C1: trait-impl blocks have zero generic support, and no fixture exercises them.** That's the biggest area where the principle is violated in a way that would silently break the moment Sky users write `impl<T: Clone> Clone for Wrapper<T>`.

**A4 + C4 is the second-biggest: generic-struct field access from Rust silently drops.** The accessor pipeline was rebuilt in Session 16 to collapse accessors into the function pipeline, but the populate-vs-CGU-walk channel split (Phase C from Session 11) creates a hole where generic accessors fall between channels.

**A5 + A6 (typeid + struct_infos name-only keys) are architectural bets** — they work today because nothing keys cached state by typeid alone, but they invite a class of future bug.

**Everything else is latent (B1–B7) — survives current usage but reads like a tripwire.**

Want me to draft a fix plan? The highest-leverage move would be a single fixture pair: (i) `case_generic_impl` exercising `impl<T> Clone for Wrapper<T>`, (ii) `case_generic_accessor` exercising `wrapper.<i32>.inner` from Rust. Either would immediately surface A1–A4 as concrete failures rather than audit findings.
