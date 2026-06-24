#!/bin/bash
# Generator for the opt-level sweep matrix fixtures.
# Per case × {opt 0, 1, 2, 3, s, z} × {lto nolto, thin} × {cgu 16}.
# Existing case4_o{0,1,2,os,oz} are kept (different naming scheme);
# new naming is `case<X>_o<L>_<lto>` (cgu always 16, kept implicit).

set -euo pipefail
ROOT="$(cd "$(dirname "$0")"/.. && pwd)/integration_projects/inlining"

for case in case1a case1b case2 case3 case4 case5 case6; do
  base="${case}_no_lto"
  [ -d "$ROOT/$base" ] || { echo "SKIP: $base"; continue; }
  for opt in 0 1 2 3 s z; do
    for lto in default thin; do
      lto_part="$lto"
      [ "$lto" = "default" ] && lto_part="nolto"
      name="${case}_o${opt}_${lto_part}"
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
        # opt-level: integer for numeric, string for "s"/"z"
        if [ "$opt" = "s" ] || [ "$opt" = "z" ]; then
          echo "opt-level = \"$opt\""
        else
          echo "opt-level = \"$opt\""
        fi
        [ "$lto" != "default" ] && echo "lto = \"$lto\""
        echo "codegen-units = 16"
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
count=$(ls "$ROOT" | grep -E "_o[0-9sz]+_(nolto|thin)$" | wc -l | tr -d ' ')
echo "DONE — $count opt-level matrix fixtures present"
