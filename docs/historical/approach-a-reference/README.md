# Approach A reference (pre-stage-3 snapshot)

Historical snapshot of the **Instance-keyed `per_instance_mir`** mechanism erw used
before commit `bf770ae` ("Stage 3 merged step 2+3") migrated to a DefId-keyed
`optimized_mir` override.

Sky needs Approach A back (see `course-correct.md` item #1 and
`rust-interop-architecture.md` §3.1, §19.1). This directory exists so the next
implementer doesn't have to dig through git archaeology to remember what the
working mechanism looked like.

---

## Files

| File | Source | Lines | What it shows |
|---|---|---|---|
| `per_instance.rs` | `bf770ae^:rustc-lang-facade/src/queries/per_instance.rs` | 216 | **The Approach A provider itself.** Deleted wholesale in `bf770ae`. Contains `lang_per_instance_mir(tcx, instance) -> Option<&Body>` plus `build_dependency_body` (which `optimized_mir.rs` ported "byte-for-byte" — see today's `queries/optimized_mir.rs` lines 102–219). |
| `queries-mod.rs` | `bf770ae^:rustc-lang-facade/src/queries/mod.rs` | 32 | **Query-table wiring.** Shows `providers.per_instance_mir = per_instance::lang_per_instance_mir;` — the line that installs the Approach A provider into rustc's query table. Today this slot is `providers.optimized_mir = optimized_mir::lang_optimized_mir;`. |
| `lib.rs` | `bf770ae^:rustc-lang-facade/src/lib.rs` | 632 | **`install_query_defaults` signature with 3 (not 4) provider params.** Shows how the OnceLock-based default-saver was structured before `DEFAULT_OPTIMIZED_MIR` was added. Useful as the wiring template. |
| `lib-instance-keyed-callback.rs` | `ed2e692^:rustc-lang-facade/src/lib.rs` | 504 | **The trait method when it was Instance-keyed.** Earlier than the other three. Look here for `LangCallbacks::collect_generic_rust_deps` carrying an `instance: ty::Instance<'tcx>` parameter — Sky wants this Instance back, but the file from `bf770ae^` already has it stripped. |

---

## How to use this when rebuilding Approach A for Sky

These files are a **structural template, not a literal answer key**. Three
important differences between erw's Approach A and what Sky needs:

1. **Query slot.** erw's Approach A rode on a custom forked `per_instance_mir`
   query whose key happened to be `LocalDefId` internally (see `per_instance.rs`
   — `lang_per_instance_mir(tcx, instance)` extracts `def_id` early and keys
   most work on it). Sky wants the query **genuinely Instance-keyed all the
   way down**, per the architecture doc §3.2 patch 1:
   `per_instance_mir(Instance<'tcx>) -> Option<&Body<'tcx>>`. Reuse the
   provider *shape* but treat `instance.args` as load-bearing concrete data
   from the start.

2. **Comptime substitution.** erw substituted `instance.args` directly because
   toylang's generics are all rustc-expressible types. Sky's per_instance_mir
   must run **Sky's comptime evaluator** on `instance.args` first, materializing
   any Sky-typed comptime values from slab pointers, *before* building the MIR
   body. This is the entire reason Sky can't use Approach B (§3.1). Slot the
   evaluation in between "Sky's universe lookup" and "build_dependency_body."

3. **`__lang_stubs` → marker-based detection.** All three files use
   `is_from_lang_stubs(tcx, def_id)` (crate-name match on `"__lang_stubs"`).
   Sky replaces this with `__SKY_STUBS_MARKER` detection per §4.5 / §6.5.
   When porting, the consumer-identification predicate's call sites stay; its
   implementation flips.

---

## Key call sites to mirror

From `per_instance.rs`:

- Lines 41–48 (filter): `if !is_from_lang_stubs_safe(...) { return None; }` — Sky's
  equivalent uses the marker check; the early-return-None shape is correct (the
  query default returns `None`, the collector falls through to `instance_mir`).
- Lines 53–65 (accessor naming): "Struct.field" callback-name construction.
  Ported byte-for-byte to today's `optimized_mir.rs`; works identically in
  Sky's Instance-keyed shape.
- Lines ~80–95 (callback dispatch): `call_collect_generic_rust_deps(...)` —
  Sky's analog returns a substituted body (or recipe), not deps-with-Params.
- Lines ~100+ (body construction): `build_dependency_body(tcx, instance, deps)`.
  In Sky, `instance.args` flows in already concrete; no `identity_for_item`
  trick needed. The MIR-construction mechanics (local decls, ReifyFnPointer
  casts, Unreachable terminator, `set_required_consts(vec![])` /
  `set_mentioned_items(vec![])`) carry over verbatim.

From `queries-mod.rs`:

- Line 30: `providers.per_instance_mir = per_instance::lang_per_instance_mir;`
  — the wiring line. Sky needs this slot to exist again (fork patch 1 declares
  the query; fork patch 3 registers the default-returns-None provider; this
  line overrides that default).

From `lib.rs`:

- `install_query_defaults` (saves the upstream default providers into OnceLocks):
  Sky adds `per_instance_mir` to the list. Note erw's pre-stage-3 list had 3
  (`layout_of`, `mir_shims`, `symbol_name`); today's list has 4
  (adds `optimized_mir`); Sky's list adds `per_instance_mir` to the 4 (or
  drops `optimized_mir` if the override is removed).

From `lib-instance-keyed-callback.rs`:

- The `LangCallbacks` trait method shape when it took an `Instance<'tcx>`.
  Sky's analog returns a pre-substituted body or a sufficient recipe; the key
  data flow (Instance in → concrete deps/body out) matches.

---

## Companion documents

- `docs/historical/handoff-optimized-mir-migration.md` — the original A → B
  migration handoff. Reading it in reverse describes what to undo.
- `course-correct.md` (repo root) item #1 — Sky-aligned summary of the pivot.
- `rust-interop-architecture.md` §3 (the fork), §13.7.5 (bounded comptime
  substitution), §19 (per_instance_mir mechanism) — the design Sky is targeting.
- Git refs: `bf770ae` (the cutover), `bf770ae^` = `ce437ae` (last commit with
  Approach A), `ed2e692^` (Instance-keyed callback), `b345162` (cleanest
  pre-stage-3 state).
