# Reply to Vale team: sidecar→sibling-cache migration shipped (toylang)

**Status:** prototype shipped. Cache is now the sole upstream metadata
artifact under the default build; sidecar emission is gated behind
`SKYC_DUAL_WRITE=1` (kept alive for A/B rollback rehearsal + the
shadow-mode CI fence) and slated for full deletion in Step 4 once the
bake period (Plan Decision 7) confirms no regressions.

---

## What shipped

The synthesis design from our exchange landed end-to-end in toylang.
Net architectural shape: **source-only distribution + local
sibling-cache at `target/<triple>/<profile>/deps/lib<crate>-<hash>.sky-cache`**,
co-located with cargo's own rlib/rmeta. Cache file shape:

```
offset  size   field
------  ----   -----
  0      4     magic "SKYC"
  4      4     cache_format_version (u32 LE)
  8     16     cache_key_digest (BLAKE3-truncated Merkle, 16 bytes)
 24      8     payload_offset (u64 LE) = 64
 32      8     payload_length (u64 LE)
 40      8     payload_checksum (BLAKE3-trunc to 8 bytes)
 48     16     reserved (zeroed)
 64      N     payload (bincode-encoded ToylangRegistry)
```

Cache content is the same `ToylangRegistry` payload the sidecar
shipped — typed AST, typeid table, cross-crate refs, source
positions — just in a different wrapper at a different location, with
a self-verifying digest in the header.

## The 10 decisions table (locked outcomes)

| # | Decision | Final outcome |
|---|----------|---------------|
| 1 | Cross-crate invalidation strategy | **Option 1 transitive Merkle fingerprinting** (your team's recommendation). Upstream cache-entry digest contributes to downstream cache key. Cargo/rustc-aligned, conservative-but-obviously-correct. |
| 2 | Cache writer location | **Eager producer-side** at upstream's own compile. Downstream cache miss = hard error mirroring §7.6's existing missing-sidecar policy. Sidesteps the GCMLZ deadlock that lazy consumer-side population would have introduced. |
| 3 | Cache key inputs | **7-axis Merkle digest** (trimmed from the original 9-input list per round-2 validation): skyc binary hash, format version, local source hashes, upstream cache digests, target triple, sky.toml hash, annotation file hashes. `cargo_lock_hash` and consumer-resolved features dropped — consumer can't reconstruct them and cargo encodes them already. |
| 4 | Cache location | **`target/<triple>/<profile>/deps/lib<crate>-<hash>.sky-cache`** — co-located with cargo's `.rlib`/`.rmeta`. Hash in filename rides cargo's normal invalidation. (Reverted from the brief embed-in-rlib pivot — that pivot was decisively killed by the pipelining/`cargo check` blocker your team's surveys would have caught.) |
| 5 | CI persistence | **No persistence; hermetic CI**. Locked per your team's explicit "want honest cold-build cost picture during prototype" guidance. |
| 6a | Sibling cache file | **Carries content, not a pointer**. The earlier "breadcrumb pointer to source root" idea wouldn't have solved the pipelining blocker. |
| 6b | `format_version` location | **In the cache file's header magic bytes**, not in `Cargo.toml [package.metadata.skyc]`. Cargo doesn't fingerprint `[package.metadata.*]` and consumer has no clean path to upstream's Cargo.toml mid-compile. |
| 7 | Migration gating | **Conservative with rollback discipline**, 4 steps with explicit `// delete after step 4` markers on every intermediate scaffold. Cleanup audit at Step 4. |
| 8 | CI fences | **All five shipped**: cache-key axis mutation, cache determinism, `SKYC_CACHE_VERIFY=1` shadow mode, trait-impl-on-Sky-type via cache path (Fence 4 = case6 retargeted), cold-CI bench script (one-shot informational). |
| 9 | Closed-source distribution | **Out of scope**. Cache file format kept shape-compatible with hypothetical future shipped blob if reconsidered (decoupled "key derivation" from "payload format"). |
| 10 | §13.4 doc model | **Single-class comptime**: all comptime evaluates at per_instance_mir time. Eager typing-time eval is a v2 optimization, not implemented in v1. Vacuously satisfied under current toylang (no comptime fixtures); producer-side lints from the validation rounds slot in as forward-defense when comptime arrives. |

## The decisive design moves your team's input drove

1. **The O(N²) re-typecheck multiplier we walked through together** killed the "literal no-cache" version of the proposal and motivated keeping local amortization. The cache lives, just not in the published `.crate`.

2. **Your team's three-options framing for cross-crate invalidation** is what landed as Decision 1. Option 1 (transitive Merkle) is the cargo-aligned pattern; we adopted it as the prototype default, with Option 2 (verify-on-load) deliberately left on the table for v2 if bench data shows the Option-1 conservatism is too aggressive.

3. **The pipelining + `cargo check` blocker** the round-2 validation surfaced is the load-bearing reason the design isn't embed-in-rlib. Sibling files at cargo's deps/ dir solve it cleanly.

## CI fence coverage (the five Fences from Decision 8)

Each fence is a named test the user-facing CI gates on:

- **Fence 1: cache-key axis mutation** (`toylangc/tests/cache_key_axis_fence.rs`). Asserts the build.rs emission covers every axis + locks `EXPECTED_AXIS_COUNT` so adding a new axis without updating both the digest derivation AND the build.rs template fails CI loudly.
- **Fence 2: cache byte determinism** (`toylangc/tests/cache_determinism.rs`). Two clean builds of the `arithmetic` fixture into per-run target dirs, byte-compare the resulting `.sky-cache` files. Mismatch = a non-deterministic source has been introduced.
- **Fence 3: `SKYC_CACHE_VERIFY=1` shadow mode** (`toylangc/tests/cache_shadow_mode.rs` + facade-side shadow check in `driver.rs::load_upstream_sidecars`). When set, the facade loads BOTH cache and sidecar, byte-compares the post-header payloads, panics on divergence with a crate-named diagnostic.
- **Fence 4: trait-impl-on-Sky-type via cache path** (`toylangc/tests/cache_trait_impl_fence.rs`). The case6 / case 6 of arch §2's taxonomy — Sky type's trait-impl method reached via the cache load path. The single most adversarial cache-scope test.
- **Fence 5: cold-CI vs sidecar timing** (`toylangc/tests/scripts/run_cache_vs_sidecar_bench.sh`). One-shot script (not gating, per the hermetic-CI policy), informational only.

Plus a meta-fence: the `cache_sidecar_equivalence.rs` test that walks a real build's emitted files and asserts both header magics are correct + lengths are well-formed.

## What you asked about during the exchange that's now answered

- **"do we really need phase K if toylang doesnt do comptime yet?"** — confirmed: Phase K is NOT a prereq. Vacuously satisfied under current toylang. The producer-side lint sits dormant; activates when comptime is added.
- **"would having that breadcrumb file next to the rlib solve the fatal thing?"** — yes, with content (not pointer). That's what shipped.
- **"option 1 sg. next?"** — locked. Transitive Merkle fingerprinting, the cargo-aligned shape.
- **"eager sg. next"** — locked. Producer-side write at upstream's own compile; downstream miss = hard error per §7.6.
- **"CI tests need to be hermetic. no caching between CI runs"** — locked. No CI persistence.
- **"yeah lets use the field"** — implemented. Cache format_version in the header (header bytes 4–7) — but per the round-2 validation, NOT in `Cargo.toml [package.metadata.skyc]` (cargo doesn't fingerprint that block).
- **"conservative with rollback sg. lets just make sure we clean up any intermediate cruft we accumulate"** — every intermediate scaffold (SKYC_USE_SIDECAR_FALLBACK env var, SKYC_DUAL_WRITE env var, dual-write code blocks) carries an explicit `// delete after step 4` comment. Step 4's cleanup audit greps for these and asserts none survive.
- **"more tests is better"** — all 5 fences shipped + 22 unit tests in `cache.rs` + 6 unit tests in `cache_key.rs` + the equivalence/dual-path integration test.
- **"yeah closed source ones are out of scope"** — closed-source distribution formally out of scope; cache format kept shape-compatible with hypothetical future shipped blob as cheap insurance.

## Open empirical questions for Sky proper

If/when the Sky team picks up the cache design for Sky proper:

1. **Phase G interaction**: under Sky-proper's eventual cdylib model (post-Phase-G), the cache key's skyc-identity input switches from `RUSTC_WORKSPACE_WRAPPER` mtime (toylang's path) to `cargo:rerun-if-env-changed=SKYC_VERSION` + `--cfg skyc_v_<blake3-prefix>`. Both mechanisms are supported via `CACHE_KEY_AXES` single-source-of-truth — worth confirming in the Sky-proper port.

2. **§29.A.content-hash-const-args correctness gate**: the producer-side lint hard-errors on comptime values flowing to Rust-visible const generic args until §29.A lands. Vacuously dormant under current toylang. When Sky proper adds comptime, this becomes a feature-unlock conversation, not a migration prerequisite.

3. **Cache hit rate vs Option 2**: the Option-1 transitive-Merkle conservatism may produce too many cache misses in deep-dep-graph projects (whitespace edit in upstream → cascading invalidations downstream). If your bench data on real Sky-scale workloads shows this hurts, Option 2 (verify-on-load with content-addressed refs) is the staged escape — but `CACHE_KEY_AXES` is structured so migrating from Option 1 to Option 2 doesn't require redesigning the key, just adding the verify pass.

## Bench numbers

(Will be populated from `run_cache_vs_sidecar_bench.sh` once the Step 3 bake completes — informational only per the hermetic-CI gate. Expectation per the workflow analysis: tens-of-milliseconds delta, dominated by cargo/rustc startup overhead rather than the cache/sidecar serialization layer.)

## Status snapshot

- Step 1 (dual-path): shipped, 469+ tests pass cold.
- Step 2 (cache-primary + 5 fences): shipped, all 5 fences green individually + integration suite (354 tests) passing.
- Step 3 (sidecar emission gated): shipped — sidecar emission requires `SKYC_DUAL_WRITE=1`. Cache is the sole metadata artifact by default. Bake period in progress.
- Step 4 (delete sidecar code): gated on Step 3 stable + cleanup audit. Code carries `// delete after step 4` markers throughout; deletion will retire `sidecar.rs`, the `SKYC_USE_SIDECAR_FALLBACK` / `SKYC_DUAL_WRITE` env vars, and the sidecar-emission code blocks.

Thanks for the detailed feedback throughout this exchange — your team's framing of the embed-in-rlib problem (pipelining + `cargo check`) saved a substantial rebuild that would otherwise have surfaced as a Step 3 regression.

Standing by for any questions on the prototype shape or for input on the Sky-proper port questions above.
