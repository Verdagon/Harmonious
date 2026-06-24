#!/bin/bash
# Perf bench runner. Builds all bench fixtures under
# tests/integration_projects/perf_bench/, runs each binary RUNS times,
# captures wall-clock (median of microseconds the binary prints as
# BENCH_ELAPSED_US=N) + .text size + symbol count via llvm-objdump.
#
# Output: markdown table to stdout (redirect to ./tmp/perf-bench-results.md).
# Side-effect: writes per-fixture main()-symbol disassembly to
# ./tmp/perf-bench-disasm/<fixture>.main.disasm for reviewer inspection.
#
# Usage:
#   bash toylangc/tests/scripts/run_perf_bench.sh > tmp/perf-bench-results.md

set -euo pipefail

ROOT="$(cd "$(dirname "$0")"/../.. && pwd)"
BENCH_DIR="$ROOT/tests/integration_projects/perf_bench"
CARGO_TARGET_DIR="$ROOT/target/integration-projects-cache"
TOYLANGC_BIN="$ROOT/../target/debug/toylangc"
SYSROOT="$(rustup run rustc-fork rustc --print=sysroot)"
SYSROOT_LIB="$SYSROOT/lib"
DISASM_DIR="$ROOT/../tmp/perf-bench-disasm"
RUNS=5

mkdir -p "$DISASM_DIR"

# Try to locate llvm-objdump (Homebrew LLVM, then system).
LLVM_OBJDUMP="$(command -v llvm-objdump 2>/dev/null || echo /opt/homebrew/opt/llvm/bin/llvm-objdump)"
if [ ! -x "$LLVM_OBJDUMP" ]; then
  LLVM_OBJDUMP=""
fi

build_bench() {
  local name="$1"
  local dir="$BENCH_DIR/$name"
  rm -rf "$dir/.toylang-build"
  (
    cd "$dir"
    DYLD_LIBRARY_PATH="$SYSROOT_LIB" \
    LD_LIBRARY_PATH="$SYSROOT_LIB" \
    CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    "$TOYLANGC_BIN" build > "$dir/.build.log" 2>&1
  )
}

run_bench() {
  # Run the binary RUNS times, parse BENCH_ELAPSED_US=N, print median.
  local bin="$1"
  local samples=()
  for _ in $(seq 1 $RUNS); do
    local out
    out=$(DYLD_LIBRARY_PATH="$SYSROOT_LIB" LD_LIBRARY_PATH="$SYSROOT_LIB" "$bin" 2>&1 || true)
    local us
    us=$(echo "$out" | grep -oE 'BENCH_ELAPSED_US=[0-9]+' | head -1 | cut -d= -f2)
    if [ -n "$us" ]; then
      samples+=("$us")
    fi
  done
  if [ ${#samples[@]} -eq 0 ]; then
    echo "FAIL"
    return
  fi
  # median
  printf '%s\n' "${samples[@]}" | sort -n | awk '
    {a[NR]=$1}
    END{if(NR%2){print a[(NR+1)/2]} else {print int((a[NR/2]+a[NR/2+1])/2)}}
  '
}

text_size() {
  local bin="$1"
  if [ -z "$LLVM_OBJDUMP" ]; then
    echo "n/a"; return
  fi
  local hex
  hex=$("$LLVM_OBJDUMP" -h "$bin" 2>/dev/null | awk '/__text/ {print $3; exit}')
  if [ -z "$hex" ]; then
    echo "n/a"
  else
    printf "%d\n" "0x$hex"
  fi
}

symbol_count() {
  local bin="$1"
  if [ -z "$LLVM_OBJDUMP" ]; then
    echo "n/a"; return
  fi
  "$LLVM_OBJDUMP" -t "$bin" 2>/dev/null | grep -c "^[0-9a-f]" || true
}

archive_disasm() {
  # Save main()-symbol disassembly per fixture so reviewers can verify
  # constant-fold / inlining claims directly. Empty file if llvm-objdump
  # not available.
  local bin="$1" name="$2"
  local out="$DISASM_DIR/$name.main.disasm"
  if [ -z "$LLVM_OBJDUMP" ]; then
    : > "$out"; return
  fi
  # Mach-O main symbol is `_main`. Capture the first ~200 lines (more
  # than enough for any sane main()).
  "$LLVM_OBJDUMP" -d --disassemble-symbols=_main "$bin" 2>/dev/null \
    | head -200 > "$out" || true
}

emit_row() {
  local label="$1" bench="$2"
  local bin="$CARGO_TARGET_DIR/debug/$bench"
  local us text syms us_display
  if [ ! -x "$bin" ]; then
    printf "| %s | BUILD FAIL | — | — |\n" "$label"
    return
  fi
  us=$(run_bench "$bin")
  text=$(text_size "$bin")
  syms=$(symbol_count "$bin")
  archive_disasm "$bin" "$bench"
  if [ "$us" = "FAIL" ]; then
    printf "| %s | RUN FAIL | %s | %s |\n" "$label" "$text" "$syms"
  elif [ "$us" = "0" ]; then
    # BENCH_ELAPSED_US=0 means LLVM eliminated the entire timed region
    # (constant-folded the loop to a closed form, or DCE'd the
    # accumulator). Add a visible "(folded?)" marker so future readers
    # don't confuse this with "instant execution."
    printf "| %s | 0 (folded?) | %s | %s |\n" "$label" "$text" "$syms"
  else
    printf "| %s | %s | %s | %s |\n" "$label" "$us" "$text" "$syms"
  fi
}

echo "# Sky perf bench results"
echo
echo "Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "Host: $(uname -srm)"
echo "RUNS per fixture: $RUNS (median reported)"
echo "llvm-objdump: ${LLVM_OBJDUMP:-(not found; .text size + symbol count = n/a)}"
echo "Disassembly archive: \`$DISASM_DIR/<fixture>.main.disasm\` per fixture."
echo
echo "Note on \`0 (folded?)\`: a result of 0 μs means LLVM eliminated"
echo "the timed region entirely (constant-folded the loop, or DCE'd the"
echo "accumulator). Bench rust_caller files use \`std::hint::black_box\`"
echo "on the loop's input and accumulator to defeat this, so 0 results"
echo "after the black_box pass indicate either fold-through-black_box"
echo "(unusual) or an LTO mode that's optimizing in a way black_box"
echo "doesn't reach. Inspect the per-fixture disassembly archive to"
echo "diagnose."
echo
echo "## Bench 1: \`add(a,b)\` call-boundary cost (100M iterations)"
echo
echo "Sky lib: \`export fn add(a: i32, b: i32) -> i32 { a + b }\`. Rust caller loops 100M iters."
echo "Includes Rust-only baseline (\`bench1_rust_baseline_*\`) calling \`test_helpers::bench_baseline_add\`"
echo "to measure Rust cross-crate call cost as a reference for Sky's call boundary."
echo
echo "| Config | Median elapsed (μs) | .text bytes | Symbol count |"
echo "|---|---:|---:|---:|"

for name in bench1_dev_o0_nolto bench1_dev_o1_nolto bench1_o3_nolto bench1_o3_thin bench1_o3_fat bench1_rust_baseline_o3_nolto bench1_rust_baseline_o3_thin; do
  echo "BUILDING $name..." >&2
  if build_bench "$name"; then
    emit_row "$name" "$name"
  else
    printf "| %s | BUILD FAIL | — | — |\n" "$name"
    echo "  build failed; see $BENCH_DIR/$name/.build.log" >&2
  fi
done

echo
echo "## Bench 2: work-per-call sweep (K=1, 10, 100; 10M iters)"
echo
echo "Sky lib does K units of arithmetic per call; Rust caller loops 10M iters."
echo "Compare nolto vs thin to see how the boundary cost amortizes with work size."
echo
echo "| Config | Median elapsed (μs) | .text bytes | Symbol count |"
echo "|---|---:|---:|---:|"

for name in bench2_k1_nolto bench2_k1_thin bench2_k10_nolto bench2_k10_thin bench2_k100_nolto bench2_k100_thin; do
  echo "BUILDING $name..." >&2
  if build_bench "$name"; then
    emit_row "$name" "$name"
  else
    printf "| %s | BUILD FAIL | — | — |\n" "$name"
    echo "  build failed; see $BENCH_DIR/$name/.build.log" >&2
  fi
done

echo
echo "## Bench 3: drop chain perf (10M Widget drops)"
echo
echo "Sky exports \`Widget\` + empty Drop impl + \`make_widget\`. Rust caller builds"
echo "a Vec<Widget> with 10M elements then times the drop."
echo
echo "Four-cell apples-to-apples matrix: {O0, O3} × {nolto, thin}. Note: bench3_drop_nolto"
echo "is O0 nolto (default cargo dev profile shape); the other three are explicit."
echo
echo "An earlier attempt at Sky-\`main\`-driven Bench 3 at opt-level≥1 hit the LLVM 21"
echo "BitcodeWriter bug (§25.2 B10) at the ThinLTO cross-CGU import phase. Moving the"
echo "Vec allocation to a Rust caller (current shape) sidesteps it; bench3_drop_o3_nolto"
echo "now builds and runs cleanly. The residual trigger is documented in §25.2 B10."
echo
echo "| Config | Median elapsed (μs) | .text bytes | Symbol count |"
echo "|---|---:|---:|---:|"

for name in bench3_drop_nolto bench3_drop_o0_thin bench3_drop_o3_nolto bench3_drop_thin; do
  echo "BUILDING $name..." >&2
  if build_bench "$name"; then
    emit_row "$name" "$name"
  else
    printf "| %s | BUILD FAIL | — | — |\n" "$name"
    echo "  build failed; see $BENCH_DIR/$name/.build.log" >&2
  fi
done

echo
echo "## Bench 3 pure-Rust baselines (10M Widget drops)"
echo
echo "Apples-to-apples comparisons against Sky's Bench 3. Widget is defined in Rust"
echo "source (not as a Sky export) across three structural variants:"
echo "- \`single_crate\`: Widget defined IN the user_bin's rust_caller. Intra-crate"
echo "  inlining at O3 can already eliminate the Drop body without LTO; upper bound"
echo "  on what nolto delivers in the best case."
echo "- \`cross_crate\`: Widget in the \`test_widgets\` sibling crate. The structural"
echo "  equivalent of Sky's bench3_drop_*: cross-crate Drop impl, no intra-crate"
echo "  inlining shortcut. THIS is the row to compare against Sky's 26.5×."
echo "- \`inline_never\`: WidgetNoInline in \`test_widgets\` with \`#[inline(never)]\`"
echo "  on Drop. Establishes the floor — what does the chain cost when the inliner"
echo "  literally can't help?"
echo
echo "| Config | Median elapsed (μs) | .text bytes | Symbol count |"
echo "|---|---:|---:|---:|"

for name in bench3_rust_baseline_single_crate_o3_nolto bench3_rust_baseline_single_crate_o3_thin bench3_rust_baseline_cross_crate_o3_nolto bench3_rust_baseline_cross_crate_o3_thin bench3_rust_baseline_inline_never_o3_nolto bench3_rust_baseline_inline_never_o3_thin; do
  echo "BUILDING $name..." >&2
  if build_bench "$name"; then
    emit_row "$name" "$name"
  else
    printf "| %s | BUILD FAIL | — | — |\n" "$name"
    echo "  build failed; see $BENCH_DIR/$name/.build.log" >&2
  fi
done

echo
echo "## Decision gate (per handoff.md / Bench 1)"
echo
echo "- Ratio < 2× (LTO over nolto at O3): architecture overhead is small even worst-case."
echo "- Ratio 2-10×: LTO mitigates; recommend \`[profile.dev] lto = 'thin'\` for dev."
echo "- Ratio > 10×: significant cost; consider revisiting AvailableExternally-or-equivalent."
echo "- LTO doesn't reduce ratio: ARCHITECTURE BUG — investigate before locking §5.5."
