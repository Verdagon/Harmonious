#!/bin/bash
# Generator for the Tier-1 inlining matrix fixtures (case<X>_<lto>_cgu<N>).
#
# Produces 56 fixture directories under
# `tests/integration_projects/inlining/` covering the cross-product:
#
#   7 cases (case1a, case1b, case2, case3, case4, case5, case6)
#     × 4 LTO modes (default/nolto, off, thin, fat)
#     × 2 codegen-units settings (1, 16)
#     all at -O3
#
# The fixtures use existing `case<X>_no_lto/` directories as their
# Sky-source template — copies main.toylang, expected_output.txt, and
# rust_caller.rs (if present), then writes a parametrized toylang.toml.
#
# Idempotent: re-running overwrites the generated fixture dirs in place.
# Existing fixtures with different names (case<X>_no_lto, *_thin_lto,
# *_fat_lto, *_o0..oz, *_cgus_*) are left untouched.
#
# After running this script, append matching test functions to
# tests/integration_projects.rs (see the "Tier 1 expanded matrix"
# section there for the pattern). For the assertion rules per cell,
# see handoff.md's "Test expansion plan" section.
#
# Usage:
#   bash toylangc/tests/scripts/gen_inlining_matrix.sh

set -euo pipefail
ROOT="$(cd "$(dirname "$0")"/.. && pwd)/integration_projects/inlining"

for case in case1a case1b case2 case3 case4 case5 case6; do
  base="${case}_no_lto"
  if [ ! -d "$ROOT/$base" ]; then
    echo "SKIP: base $base not found"; continue
  fi
  for lto in default off thin fat; do
    for cgu in 1 16; do
      lto_part="$lto"
      if [ "$lto" = "default" ]; then lto_part="nolto"; fi
      name="${case}_${lto_part}_cgu${cgu}"
      dir="$ROOT/$name"
      mkdir -p "$dir"
      cp "$ROOT/$base/main.toylang" "$dir/main.toylang"
      [ -f "$ROOT/$base/expected_output.txt" ] && cp "$ROOT/$base/expected_output.txt" "$dir/expected_output.txt"
      [ -f "$ROOT/$base/rust_caller.rs" ] && cp "$ROOT/$base/rust_caller.rs" "$dir/rust_caller.rs"
      {
        echo "[project]"
        echo "name = \"$name\""
        echo "source = \"main.toylang\""
        [ -f "$ROOT/$base/rust_caller.rs" ] && echo "rust_caller = \"rust_caller.rs\""
        echo "opt-level = \"3\""
        [ "$lto" != "default" ] && echo "lto = \"$lto\""
        echo "codegen-units = $cgu"
        if grep -q "^features" "$ROOT/$base/toylang.toml" 2>/dev/null; then
          grep "^features" "$ROOT/$base/toylang.toml"
        fi
        echo ""
        if grep -q "^\[rust-dependencies\]" "$ROOT/$base/toylang.toml" 2>/dev/null; then
          awk '/^\[rust-dependencies\]/{flag=1} flag{print}' "$ROOT/$base/toylang.toml"
        fi
      } > "$dir/toylang.toml"
    done
  done
done

count=$(ls "$ROOT" | grep -E "_(nolto|off|thin|fat)_cgu[0-9]+" | wc -l | tr -d ' ')
echo "DONE — $count fixtures present matching the Tier-1 matrix schema"
