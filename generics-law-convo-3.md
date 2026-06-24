# Generics-law conversation 3

Second "deep audit" ask (verbatim repeat of the first), after Phase B's uniformity sweep had landed.

---

## 👤 User

can you do a very deep audit, and dispatch many agents, to find everywhere where we're special-casing non-generic functions, and find all the places where we're not abiding by the principle "non-generic functions are just a normal case of generic functions, and should cleanly come from the design of generic functions"? find everywhere where something wouldn't work for a generic function please

---

## 🤖 Claude

Dispatching four parallel investigators across the codebase. Each agent gets a focused scope and an explicit reminder of CLAUDE.md's compiler law plus the Phase B context (we just unified the populate trait_impls channel — non-generic falls out as `concrete_args = []` through the same cascade-captured loop as generic).

---

## 🤖 Claude

Four agents reported. Synthesized findings, deduped, structured as the previous audit.

## A. Architectural — silently wrong for generic items today

| # | Site | Issue |
|---|---|---|
| **A1** | `rustc-lang-facade/src/queries/layout.rs:64` | `has_params` early-out routes unsubstituted-generic ADTs to rustc's default instead of `monomorphize_type`. Works only because stub_gen's universal `(__ToylangOpaque<HASH>, PhantomData<(P...)>)` carrier coincidentally matches sizes per the `debug_assert_eq!(.., 2, ..)` at line 153. Stub_gen refactor that drops the unused `PhantomData<()>` for N=0 would silently mis-report field offsets. Right shape: drop the gate; let `call_monomorphize_type` decide. |

## B. Latent — works today, breaks with one refactor

| # | Site | Issue |
|---|---|---|
| **B1** | `toylangc/src/toylang/callbacks_impl.rs:1659-1679` (`walk_and_stash_internal_callees` push) | Pushes `ToylangInstance { stub_def_id: None, instance_args: vec![] }`. The `internal_symbol` correctly mangles `type_args`, but discards them on the push. Works only because `stub_def_id: None` ⇒ Instance never reconstructed. The moment ABI lookup needs `stub_def_id` for an internal-only generic chain, the args are lost. |
| **B2** | `callbacks_impl.rs:419` + `:485` (export + accessor push sites) | `Instance::new_raw(def_id, GenericArgs::empty())` + hardcoded `instance_args: vec![]`. Gated by `has_abstract_args` upstream — that gate IS the special-case. If generic exports ever become legal, removing the gate makes the line silently mis-mangle. |
| **B3** | `registry.rs:259-261` (`has_abstract_args`) | Predicate name promises "abstract args / unsubstituted Params remaining"; implementation says `!self.type_params.is_empty()` (declared-params check). After partial substitution (impl-block bound, method-level free), the merged Phase-B `type_params` list makes this misfire. |
| **B4** | `registry.rs:213` (`self_type_args: Vec<String>`) + parser/stub_gen consumers | Name-only representation can't encode `impl<T> Trait for Foo<T, i32>` (partial concrete) or `impl Trait for Foo<&T>` (ref/composite). |
| **B5** | `registry.rs:208` (`type_param_bounds: Vec<(String, String)>`) | Bound is name-only — can't express `T: Iterator<Item=i32>` or `T: PartialEq<U>` (bounds with their own type args). |
| **B6** | `oracle.rs:693-724` (`find_trait_impl_method_def_id`) | Matches by self-type-name only; no check that user-supplied `concrete_args` match the impl's self-type pattern. Toylang could synthesize an unsound Instance for `impl Trait for Wrapper<i32>` with `concrete_args=[i64]`. |
| **B7** | `rustc-lang-facade/src/queries/upstream_monomorphization.rs:46-49` | `map.entry(def_id).or_default().insert(args, crate_num)` — no debug check that a stale-sidecar collision with a different `CrateNum` is being silently overwritten. Worst case for `concrete_args=[]` (N=0 degenerate). |
| **B8** | `upstream_monomorphization.rs:74-78` (Phase-6 wrapper `None` guard) | Asks "are you a non-trait-impl lang-stub?" — coincides with "Phase-6 wrapper" only because no other non-generic non-trait-impl lang-stub kind exists yet. Future `__toylang_*` shim would be force-`None`'d incorrectly. Real predicate is `is_phase6_wrapper`. |
| **B9** | `rustc-lang-facade/src/queries/per_instance.rs:60-78` callback-name shape | Produces `Struct.field` for accessor-like deps, never `__impl_method__<S>__<T>__<m>` for trait-impl-method deps. `symbol_name.rs` DOES produce the trait-impl form. Asymmetric between facade sites. Probably safe (trait-impl methods may not reach `per_instance_mir`), but un-asserted. |
| **B10** | `callbacks_impl.rs:1256-1263` (crate-name → CrateNum loop in synthesize) | First-wins on duplicate crate-name; concrete_args=[] entries from different upstreams indistinguishable. |

## C. Vestigial / cleanup-worthy

| # | Site | Cleanup |
|---|---|---|
| **C1** | `callbacks_impl.rs:435, 501, 1674` hardcoded `instance_args: vec![]` | Asymmetric with the discovered-instances loop (538-580) that builds from captures. Funnel all push sites through one shape — would also retire B1+B2. |
| **C2** | `callbacks_impl.rs:429` `compute_internal_symbol_from_type_args(name, &[])` | Hardcoded `&[]` is the N=0 evaluation of the looping version at :567. Share a helper. |
| **C3** | `type_resolve.rs:378-450` (`Expr::FnCall` arm) | Two near-duplicate branches gated on `!func.type_params.is_empty()`. The non-generic branch IS the empty-subst degenerate case. Build empty `type_arg_subst` when N=0, run the generic branch unconditionally, delete the else. Current fence-allow label "arity check" is **wrong** — relabel or fix. |
| **C4** | `oracle.rs:397-402` (`resolved_type_to_mangled_name`) | `Foo` vs `Foo_LT_..._GT_` — could always emit the trailing marker. Cosmetic. |
| **C5** | `codegen_wrapper.rs` (whole file) | Module docstring already self-identifies as vestigial. Pure delegate post-Tier 3 #12. |
| **C6** | `queries/drop_glue.rs:38-43` | `consumer_struct_name` discards `args` silently — sound (one Drop per type-name contract), but the `_` deserves a comment. |

## D. Justified asymmetries (keep)

- `stub_gen.rs:14, 28, 94, 369, 388` — empty token streams (Rust syntax forbids `impl<>` / `Foo<>` / `Self<>`).
- `per_instance.rs:132-144` — `debug_assert!(!instance.args.has_param())` — load-bearing Approach A invariant.
- `lib.rs:702, 729, 82` — `instantiate_identity` for structural ADT-name reads (Param placeholders ignored by the `Adt(adt_def, _)` match).

## E. Coverage gaps

Patterns the code path SHOULD support but no fixture exercises:

| # | Pattern | Minimal fixture |
|---|---|---|
| **E1** | Method-level type params on impl methods (combinatoric impl-block + method) | `impl<T> Wrapper<T> { fn map<U>(self, u: U) -> Pair<T, U> }` called at distinct `(T, U)` |
| **E2** | Inherent impl on generic struct | `impl<T> Wrapper<T> { fn unwrap(self) -> T }` at two T |
| **E3** | Multi-parameter impl block | `impl<A: Clone, B: Clone> Clone for Pair<A, B>` + `duplicate<Pair<i32, i64>>` |
| **E4** | Generic accessor from Rust caller | `wrapper_i32.inner` from `rust_caller.rs` |
| **E5** | Sky generic free fn shared by Rust AND Sky callers | case1b extended — Sky `main` also calls `identity::<i32>` |
| **E6** | Generic struct field whose type involves T non-trivially | `Holder<T> { items: Vec<T, Global> }` at two T |
| **E7** | Two bounds on impl param | `impl<T: Clone + Debug> Trait for Foo<T>` |
| **E8** | Partial concrete self-type args (B4 surface) | `impl<T> Trait for Foo<T, i32>` |
| **E9** | Generic-struct-of-generic-struct (recursive substitution) | `Wrapper<Box<i32>>` both Sky |
| **E10** | Same generic struct at distinct types in one program — drop dispatch | `Wrapper<i32>` + `Wrapper<i64>` both with non-trivial drops |

## Honest read on the bands

**A** has exactly one live finding (A1) and the fix is small — drop the `has_params` gate and let `monomorphize_type` handle uniformly. The Phase A2 layout unification did half the work; A1 is the missing half.

**B** clusters around two themes:
- *Single-element-vs-list dropping*: B1 (walker discards type_args), B4/B5 (self-type-args and bounds are name-only Strings, can't represent richer Rust types).
- *Predicate-vs-implementation drift*: B3 (`has_abstract_args` lies), B6 (impl-method lookup doesn't validate args), B8 (Phase-6 guard is shape-of-item, not identity-of-item), B9 (callback name shape asymmetric across facade sites).

**C1** is the biggest cleanup win — three hardcoded `vec![]` push sites should funnel through the same discovered-instances shape we already wrote. That retires B1 + B2 as a side effect.

**E1, E2, E3** are the highest-priority new fixtures — each exercises a code path that Phase B touched but only at the narrowest version. Adding them would CI-fence the compiler-law unification at the broader axes too.

Want me to:
1. **Fix A1** (drop the layout has_params gate) — small, contained.
2. **Land C1+C3** (unify the three populate push sites and the type_resolve duplicate branches) — bigger, retires B1/B2/B3 side-effect-cleanly.
3. **Add fixtures E1/E2/E3** — small fixtures, high CI value.
4. **All three** as a follow-up arc, similar shape to today's Phase B.
