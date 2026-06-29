#!/usr/bin/env bash
# Fence 5 (Sidecar→cache migration, Step 2):
# Cold-CI cache vs sidecar timing.
#
# One-shot bench (NOT wired into `cargo test`, matches the existing
# `run_perf_bench.sh` pattern). Builds the `arithmetic` fixture under
# both `SKYC_USE_SIDECAR_FALLBACK=true` (sidecar path) and the
# default (cache path), 5 runs each, reports median wall-clock.
#
# Informational only — does NOT gate any CI job (user-locked
# hermetic-CI policy means we don't cache between runs, so cold cost
# is the cost). The number is for context when deciding whether the
# cache work pays off vs the sidecar path it replaces.
#
# Usage:
#   bash toylangc/tests/scripts/run_cache_vs_sidecar_bench.sh \
#     > tmp/cache-vs-sidecar-results.md
#
# Output format mirrors `run_perf_bench.sh`'s markdown table.
#
# Prerequisites: same as run_perf_bench.sh (rustc-fork toolchain,
# toylangc binary at target/debug/toylangc, llvm-objdump on $PATH).

set -euo pipefail

# Resolve paths relative to this script.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/toylangc/tests/integration_projects/arithmetic"
TOYLANGC="$REPO_ROOT/target/debug/toylangc"

if [[ ! -x "$TOYLANGC" ]]; then
  echo "error: toylangc binary not found at $TOYLANGC" >&2
  echo "       run: LLVM_SYS_211_PREFIX=... cargo build --bin toylangc" >&2
  exit 1
fi

SYSROOT="$(rustup run rustc-fork rustc --print=sysroot)"
export DYLD_LIBRARY_PATH="$SYSROOT/lib"
export LD_LIBRARY_PATH="$SYSROOT/lib"

# Run a single build, return wall-clock in milliseconds.
# $1 = label (cache | sidecar)
# $2 = target dir
# $3 = env var name to set ("" for default cache-primary)
time_one_build() {
  local label="$1"
  local target_dir="$2"
  local env_var="$3"

  rm -rf "$FIXTURE_DIR/.toylang-build" "$target_dir"
  local start_ms
  start_ms=$(python3 -c 'import time; print(int(time.time()*1000))')

  if [[ -n "$env_var" ]]; then
    (
      cd "$FIXTURE_DIR"
      CARGO_TARGET_DIR="$target_dir" \
      "$env_var"=1 \
      "$TOYLANGC" build >/dev/null 2>&1
    )
  else
    (
      cd "$FIXTURE_DIR"
      CARGO_TARGET_DIR="$target_dir" \
      "$TOYLANGC" build >/dev/null 2>&1
    )
  fi

  local end_ms
  end_ms=$(python3 -c 'import time; print(int(time.time()*1000))')
  echo $((end_ms - start_ms))
}

# Median of 5 runs.
# $1 = label
# $2 = env var ("" for default)
median_of_5() {
  local label="$1"
  local env_var="$2"
  local target_dir="$REPO_ROOT/target/cache-bench-${label}"
  local times=()
  for i in 1 2 3 4 5; do
    times+=("$(time_one_build "$label" "$target_dir" "$env_var")")
  done
  # Sort + pick median (index 2 of 5)
  IFS=$'\n' sorted=($(sort -n <<<"${times[*]}")); unset IFS
  echo "${sorted[2]}"
}

echo "# Cache vs Sidecar Cold-Build Timing (Step 2 Fence 5)"
echo
echo "Built with toylangc=$TOYLANGC"
echo "Fixture: $FIXTURE_DIR"
echo "Method: 5-run median per configuration, wall-clock ms"
echo
echo "| Configuration | Median (ms) |"
echo "|---|---:|"

CACHE_MS=$(median_of_5 "cache-primary" "")
echo "| Cache-primary (Step 2 default) | $CACHE_MS |"

SIDECAR_MS=$(median_of_5 "sidecar-fallback" "SKYC_USE_SIDECAR_FALLBACK")
echo "| Sidecar fallback (A/B rollback path) | $SIDECAR_MS |"

# Compute ratio safely.
if (( SIDECAR_MS > 0 )); then
  RATIO=$(python3 -c "print(f'{$CACHE_MS / $SIDECAR_MS:.2f}')")
  echo
  echo "Cache/Sidecar ratio: ${RATIO}x"
fi

echo
echo "## Interpretation"
echo
echo "These are cold-build wall-clock times; cargo's incremental cache"
echo "is wiped between every run. Differences this small are dominated"
echo "by cargo/rustc startup overhead and Rust dep compilation, not by"
echo "the sky-cache vs sky-meta serialization layer (both are ~1ms)."
echo
echo "The number exists to detect a multi-second regression (e.g. cache"
echo "key axis computation becomes O(n*log(n)) in source size). For"
echo "warm-build perf, see run_perf_bench.sh."
