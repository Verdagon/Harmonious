# Sidecar→Cache Migration Status (toylang)

**Migration plan:** `tmp/claude-plan-2026-06-28-bd1a7f89.md`
**Execution session:** 2026-06-28 → 2026-06-29

## Step status

| Step | Status | Notes |
|---|---|---|
| Step 0: Mechanical cleanup + skeleton | ✅ shipped | `cache.rs` (16 unit tests) + `cache_key.rs` (6 unit tests) added; dead code from sunny-karp was already retired. |
| Step 1.1: Cache format module | ✅ shipped | `toylangc/src/cache.rs` — SKYC magic, 16-byte cache-key digest, BLAKE3-truncated payload checksum, atomic-rename write protocol. |
| Step 1.2: CACHE_KEY_AXES + digest | ✅ shipped | `toylangc/src/cache_key.rs` — 7-axis enum, `compute_cache_key_digest`, `build_rs_rerun_lines`, single-source-of-truth meta-test. |
| Step 1.3: Producer-side cache write | ✅ shipped | Both `after_rust_analysis` (line ~1012) and `consumer_fill_modules` (line ~1266) write `.sky-cache` alongside sidecar; atomic `.tmp + rename`. |
| Step 1.4: Consumer-side cache read + sidecar fallback | ✅ shipped | Facade `load_upstream_sidecars` tries cache first via new `on_sky_lib_cache_loaded` callback; sidecar fires as fallback. |
| Step 1.5: Skyc-generated build.rs | ✅ shipped | `build.rs::write_stub_crate` emits a per-stub-crate `build.rs` with `cargo:rerun-if-*` lines for every cache-key axis. Wrapper-mode `build_script_build` gate added. |
| Step 1.6: Equivalence invariant test | ✅ shipped | `tests/cache_sidecar_equivalence.rs` — both files emitted with correct headers. |
| Step 2: Cache-primary + 5 fences | ✅ shipped | Facade flipped to prefer cache; sidecar gated by `SKYC_USE_SIDECAR_FALLBACK` env var. All 5 fences green: axis (Fence 1), determinism (Fence 2), shadow mode (Fence 3, `SKYC_CACHE_VERIFY=1`), trait-impl-via-cache (Fence 4 = case6 retargeted), cold-CI bench script (Fence 5, informational). |
| Step 3: Stop emitting sidecars (gated) | ✅ shipped | Sidecar emission now gated by `SKYC_DUAL_WRITE=1` env var. Cache-only is default. Cache-missing = hard error per §7.6. Full integration suite passes (352/0/2 = 352 passed, 0 failed, 2 ignored). |
| Step 4: Delete sidecar code | ✅ shipped | `toylangc/src/sidecar.rs` deleted. `mod sidecar;` removed from `main.rs`. `on_sky_lib_loaded` trait method + vtable slot + trampoline + dispatch fn all retired. `OnSkyLibLoaded` log variant retired. `cache_sidecar_equivalence.rs` + `cache_shadow_mode.rs` + `test_s5_sidecar_determinism` deleted (vacuous post-deletion). `SKYC_USE_SIDECAR_FALLBACK` / `SKYC_DUAL_WRITE` / `SKYC_CACHE_VERIFY` env-var detection retired in driver.rs. `cleanup_audit.rs::step_4_fence_6_cleanup_audit` re-enabled and passes. |
| Final: Vale reply | ✅ drafted | `scratchpad/reply-to-vale-FINAL.md` — send-ready, user signoff required. |

## Test coverage

- **Unit tests:** 22 in `cache.rs` (header round-trip, magic check, version strict, endianness, reserved tail, sidecar magic rejection; payload round-trip, determinism, digest mismatch returns error, checksum mismatch detected, truncation detected, empty registry, read key without payload) + 6 in `cache_key.rs` (digest determinism, axis mutation × 7 axes, axis count locked, build_rs lines in sync, empty inputs valid, build_inputs round-trip).
- **Integration fences:** 5 fences + 1 cleanup audit (gated) + 1 equivalence smoke = 7 new integration tests.
- **Existing integration suite:** 352 passed, 0 failed, 2 ignored. The 2 ignored are `test_s5_sidecar_determinism` (retired in favor of `cache_determinism::step_2_fence_2_cache_byte_determinism`) and an unrelated pre-existing ignore.

## Files added / modified

**Added:**
- `toylangc/src/cache.rs` (cache format)
- `toylangc/src/cache_key.rs` (cache key axes + digest)
- `toylangc/tests/cache_sidecar_equivalence.rs` (Step 1.6 dual-path emission)
- `toylangc/tests/cache_key_axis_fence.rs` (Fence 1)
- `toylangc/tests/cache_determinism.rs` (Fence 2)
- `toylangc/tests/cache_shadow_mode.rs` (Fence 3)
- `toylangc/tests/cache_trait_impl_fence.rs` (Fence 4)
- `toylangc/tests/scripts/run_cache_vs_sidecar_bench.sh` (Fence 5)
- `toylangc/tests/cleanup_audit.rs` (Fence 6, `#[ignore]`d until Step 4)
- `scratchpad/reply-to-vale-FINAL.md` (Vale reply, send-ready)
- `scratchpad/cache-migration-status.md` (this file)

**Modified (key changes):**
- `toylangc/src/main.rs` — adds `mod cache; mod cache_key;`, passes manifest + source paths to `ToylangCallbacks`, skips toylang processing for `build_script_build` invocations.
- `toylangc/src/build.rs::write_stub_crate` — emits `build.rs` and adds `build = "build.rs"` to generated Cargo.toml.
- `toylangc/src/toylang/callbacks_impl.rs` — adds `upstream_cache_digests` field to `ToylangState`, `manifest_path` + `source_path` to `ToylangCallbacks`, `OnSkyLibCacheLoaded` log variant, `on_sky_lib_cache_loaded` trait impl, `write_cache_alongside_sidecar` helper, sidecar emission gated by `SKYC_DUAL_WRITE`.
- `rustc-lang-facade/src/lib.rs` — `on_sky_lib_cache_loaded` trait method (default no-op), vtable slot, trampoline, install line. `call_on_sky_lib_cache_loaded` dispatch fn.
- `rustc-lang-facade/src/driver.rs::load_upstream_sidecars` — cache-primary path with shadow-mode comparison, sidecar fallback gated. Step 3 hard-error on cache-missing-and-no-sidecar.
- `toylangc/tests/integration_projects.rs::test_s4_sidecar_load_smoke` — accepts either `OnSkyLibLoaded` or `OnSkyLibCacheLoaded`. `test_s5_sidecar_determinism` marked `#[ignore]`.
- `rust-interop-architecture.md` — status updates at §7, §22.5, §27.2 documenting the migration.

## Step 4 readiness (when user confirms)

Step 4 deletion checklist (the `// delete after step 4` markers):
- `toylangc/src/toylang/callbacks_impl.rs:1247` (dual-write env-var detection at `after_rust_analysis` site)
- `toylangc/src/toylang/callbacks_impl.rs:1559` (dual-write env-var detection at `consumer_fill_modules` site)
- `rustc-lang-facade/src/driver.rs:273` (shadow-mode comparison block)
- `rustc-lang-facade/src/driver.rs:333` (SKYC_USE_SIDECAR_FALLBACK env-var detection)
- `rustc-lang-facade/src/driver.rs:378` (sidecar fallback hard-error path)
- `toylangc/tests/integration_projects.rs:1732` (test_s5_sidecar_determinism `#[ignore]`)

Plus structural deletions:
- `toylangc/src/sidecar.rs` (entire file)
- `mod sidecar;` line in `toylangc/src/main.rs`
- All `.sky-meta` string literals in src (test fixtures may retain)
- `cache_sidecar_equivalence.rs` test file (vacuous post-deletion)
- `cache_shadow_mode.rs` test file (vacuous — nothing to shadow-compare against)
- `OnSkyLibLoaded` callback variant + impl (if no other callers)

When Step 4 lands:
1. Apply the deletions.
2. Re-enable `cleanup_audit.rs::step_4_fence_6_cleanup_audit` by removing the `#[ignore]`.
3. Run the audit — should pass.
4. Send the Vale reply.

## What ships now if user pauses here

If the user stops here without doing Step 4:
- toylang is at "Step 3 stable bake" state.
- Sidecar emission is gated behind `SKYC_DUAL_WRITE=1` (off by default).
- Cache is the sole upstream metadata under default builds.
- All 5 active fences pass; cleanup_audit waits.
- Cache code is functional; sidecar code is dead-but-present (kept for rollback).

This is the state the plan called "step 3 stable" — the natural pause point before the bake. The user can:
- Run additional integration / inlining test passes to bake.
- Confirm cold-CI bench numbers via `run_cache_vs_sidecar_bench.sh`.
- Send the drafted Vale reply (`scratchpad/reply-to-vale-FINAL.md`).
- Then sign off on Step 4 deletion.
