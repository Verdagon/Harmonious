---
name: collapsed-call-tree
description: Investigate a bug by building a collapsed call tree with debug printouts, iteratively narrowing down root causes.
---

# Collapsed Call Tree Investigation

Technique for investigating compiler bugs by tracing execution and building a human-readable collapsed call tree.

## Process

1. **Add debug printouts** to key functions along the suspected execution path. Print function name, key arguments, and outcomes (success/failure/which branch taken).

2. **Run the specific failing test**, always teeing output to the **same** file in `/tmp/` for grep-based analysis. Reuse one file (e.g. `/tmp/trace.txt`) across all runs so you don't lose track. **Always run cargo and grep as separate commands** — first tee, then grep the file:
   ```
   cargo test --test integration_tests test_name -- --nocapture 2>&1 | tee /tmp/trace.txt
   ```
   Then analyze separately:
   ```
   grep DEBUG /tmp/trace.txt | head -20
   grep FAILED /tmp/trace.txt
   ```
   Never pipe cargo output directly into grep — the pipe can swallow output, and you lose the ability to re-analyze without re-running.

### Getting cargo to show output

- **Use `--nocapture`** to see println/eprintln output from tests: `cargo test test_name -- --nocapture`.
- **Run a single test** to keep output manageable:
  ```
  cargo test --test integration_tests test_name -- --nocapture 2>&1 | tee /tmp/trace.txt
  ```
- **If you add printlns that don't fire**, it means that code path isn't reached — which is itself useful information. Note it in the investigation doc and instrument higher up.

3. **Build the collapsed call tree** in a markdown file in the repo. Format:
   ```
   - outer_func() ... inner_func():
     What happened and why it matters.
   - outer_func2() ... deep_func():
     What went wrong here.
   ```
   Collapse intermediate calls with `...`. Include only nodes where something interesting happens (branches taken, values decided, errors produced). Use `file.rs::fn_name` for locations (no line numbers — they shift as you add printouts).

4. **Iterate**: each round of debug printouts answers questions and raises new ones. Add findings to the markdown file, add more printouts, rerun. Keep going until root cause is clear. **Periodically show the updated investigation doc to the user** so they can follow along and steer.

5. **STOP before implementing any fix.** Show the investigation doc to the user and explicitly ask for approval before writing any fix code. The user always wants to read the findings and understand the root cause before any code changes are made. **Never jump from diagnosis to fix** — not even a "quick" one. Undo any speculative fix attempts if you made them during investigation. The only code changes allowed during investigation are debug printouts.

6. **Clean up** debug printouts when done.

## Example

Bug: `sort(list)` corrupts state, causing an assertion later in `validate()`.

### Collapsed call tree

```
- codegen::process_function("main") calls:
  - codegen::compile_sort_call() calls ... calls codegen::emit_sort_ops():
    LIKELY FACTOR: reverses element order in rune table — uses `>` instead of `<`.
  - validator::validate_body() calls ... calls validator::check_rune_table():
    Asserts rune table is sorted ascending. Fails because emit_sort_ops left it descending.
```

Only include nodes relevant to the bug — skip uninteresting intermediate calls.

## Tips & Tricks

- **Absent output IS the finding.** If a println doesn't fire, that code path isn't reached — which often IS the root cause. Don't waste time debugging the println; note it in the investigation doc and instrument higher up.
- **Start with a canary println.** Before adding printouts on suspected paths, put one in a function you KNOW fires (e.g. one that already printed in earlier rounds). If the canary fires but your new ones don't, you've confirmed the code path isn't reached — not that cargo is broken.
- **Check your assumptions about WHAT is failing.** If the error says `CouldntFindFunctionToCallT`, immediately check which function name it's looking for. The error might be about `drop`, not the function you're investigating. Print identifying info from the error object first.

## Rules

- Don't include file line numbers (they change as you add printouts). Use file + definition name.
- If you see something impossible, stop and flag it.
- Keep the markdown file updated as you go — it's the living investigation document.
- Tee test output to `/tmp/`, then grep. Don't try to read raw cargo output.
- Put investigation markdown files in an `investigations/` subdirectory.
- Err on the side of leaving debug printouts in — only remove ones that are actively confusing. Clean up all of them when the investigation is complete.
- Start from the outermost entry point (e.g. expression compiler for a function call) and trace inward.
