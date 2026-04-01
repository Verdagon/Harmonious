# Design: Triggering Rust Monomorphization from Toylang

## Problem

When a Toylang function (compiled by Toylang's own LLVM backend) calls Rust generic
functions — e.g. `Vec::<Point>::new()`, `Vec::<Point>::push()` — rustc must generate
machine code for those specific instantiations. Rustc's monomorphization collector discovers
what to generate by walking the MIR call graph. If the Toylang function's MIR body is just
a stub that calls an external symbol (`__toylang_impl_make_vec`), the collector never sees
the `Vec::new` or `Vec::push` calls, so it never generates them. The Toylang-compiled object
file then references undefined symbols and the linker fails.

Two approaches can solve this. Both produce the same outcome — rustc generates the needed
Rust instantiations — but differ in mechanism.

---

## Approach A: Phantom Function Pointer Struct

### Mechanism

The `mir_built` override constructs MIR that takes function pointers to every Rust
instantiation the Toylang function needs, packs them into a struct, and passes a reference
to that struct as an extra argument to the external Toylang call. The Toylang-compiled
function receives the argument and ignores it.

### Example MIR body for `make_vec`

```
bb0:
    // Take function pointers to trigger monomorphization
    _1: fn() -> Vec<Point>             = const Vec::<Point>::new
    _2: fn(&mut Vec<Point>, Point)     = const Vec::<Point>::push
    // Pack into a struct
    _3: PhantomDeps                    = Aggregate { _1, _2 }
    _4: &PhantomDeps                   = &_3
    // Call external Toylang implementation with phantom arg
    _0: Vec<Point>                     = call __toylang_impl_make_vec(_4) → bb1
bb1:
    return
```

### How it triggers monomorphization

The monomorphization collector walks the MIR body's reachable basic blocks. When it
encounters a `ConstOperand` containing `Ty::new_fn_def(tcx, vec_new_def_id, &[Point])`,
it recognizes this as a reference to a concrete function instance and adds it to its
worklist. The collector then generates `Vec::<Point>::new` and `Vec::<Point>::push`.

This happens in `MirUsedCollector::visit_basic_block_data` in
`compiler/rustc_monomorphize/src/collector.rs`, which processes every `Rvalue`,
`Operand`, and `Terminator` in reachable blocks, looking for function types and
adding them as `MentionedItem::Fn` entries to the internal `used_mentioned_items` set.

### What the Toylang side does

The external function `__toylang_impl_make_vec` is compiled by Toylang's LLVM backend.
Its actual signature includes the phantom struct pointer as an extra parameter, but the
compiled code ignores it. It calls the Rust functions directly by their mangled symbol
names (which are stable for a given set of generic args on a given nightly).

### Tradeoffs

**Advantages:**
- Works through the collector's normal MIR traversal — no reliance on metadata fields.
  The function references are "real" from the collector's perspective: they appear in
  reachable basic blocks as constants.
- Items collected this way are **"used" items**, not "mentioned" items. This means they
  are fully codegen'd (added to `state.visited`, partitioned into codegen units). There
  is zero ambiguity about whether they'll end up in the final binary.
- Robust against MIR optimizations. The function pointers are assigned to locals and
  passed to a call, so they can't be optimized away as dead code (the call uses them).
- No special handling needed in `mir_promoted` or other MIR passes — the body is
  structurally valid MIR with correct types throughout.

**Disadvantages:**
- More complex MIR construction. Each stub body needs: local declarations for each
  function pointer, an aggregate struct type, a borrow, and the call. This is ~10-15
  MIR statements per stub instead of ~2.
- The phantom struct type must be defined somewhere (or synthesized as a tuple).
- The external Toylang function has an extra argument it ignores. This affects the
  calling convention: Toylang's compiler must know to emit a function that accepts
  the extra pointer even though it never reads it.
- If the function has many Rust dependencies (e.g., 20 different Vec/HashMap methods),
  the phantom struct grows large. This is not a runtime cost (the struct is stack-
  allocated and the data is never read), but it's more MIR to generate.

---

## Approach B: `mentioned_items` on the MIR Body

### Mechanism

The `mir_built` override constructs a minimal MIR body (just a call to the external
Toylang function and a return), then sets the body's `mentioned_items` field to a list
of `MentionedItem::Fn(ty)` entries for each Rust instantiation needed.

### Example MIR body for `make_vec`

```
bb0:
    _0: Vec<Point> = call __toylang_impl_make_vec() → bb1
bb1:
    return

mentioned_items: [
    MentionedItem::Fn(Ty::new_fn_def(tcx, vec_new_def_id, &[Point])),
    MentionedItem::Fn(Ty::new_fn_def(tcx, vec_push_def_id, &[Point, Global])),
]
```

### How it triggers monomorphization

After the collector processes a body's reachable basic blocks (collecting "used" items),
it processes `body.mentioned_items()` in a second pass (collector.rs lines ~1259-1264):

```rust
for item in body.mentioned_items() {
    if !collector.used_mentioned_items.contains(&item.node) {
        let item_mono = collector.monomorphize(item.node);
        visit_mentioned_item(tcx, &item_mono, item.span, &mut mentioned_items);
    }
}
```

`visit_mentioned_item` resolves `MentionedItem::Fn(ty)` to a concrete `Instance` and
adds it to the mentioned items worklist. This is then recursed into via
`collect_items_rec` with `CollectionMode::MentionedItems`.

### Critical difference: "mentioned" vs "used" items

The collector maintains two separate sets:

- **`state.visited`** ("used" items): Items that appear in reachable MIR code. These
  are guaranteed to be codegen'd. The final `collect_crate_mono_items` returns
  `state.visited.into_inner()` as the set of items to compile.

- **`state.mentioned`** ("mentioned" items): Items referenced in `mentioned_items` but
  not in reachable code. These are tracked in a separate set. They are recursed into
  (their own `mentioned_items` are transitively collected), but they go into
  `state.mentioned`, not `state.visited`.

**The key question: do "mentioned" items get codegen'd?**

Looking at the code flow:
1. `collect_crate_mono_items` returns `state.visited.into_inner()` — only "used" items.
2. `collect_and_partition_mono_items` calls `partition(tcx, items.iter())` on these items.
3. Mentioned items that are NOT also used do NOT appear in the returned item set.

However, this needs careful analysis. When `collect_items_rec` processes a mentioned item,
it calls itself recursively. If the mentioned item's own body contains "used" code (e.g.,
`Vec::<Point>::new` has a body that rustc will process normally), then the *transitive*
dependencies of the mentioned item DO get added to `state.visited`.

In practice, the flow is:
1. Our body mentions `Vec::<Point>::new` as a `MentionedItem::Fn`
2. The collector resolves it to an `Instance` and calls `collect_items_rec` in
   `MentionedItems` mode
3. In `MentionedItems` mode, the item is added to `state.mentioned` (not `visited`)
4. BUT: if `Vec::<Point>::new` is also transitively reachable from some "used" root
   (which it typically is, since our stub body calls `__toylang_impl_make_vec` which
   at the linker level calls `Vec::new`), then it gets added to `visited` through
   that other path

**The risk scenario:** if the Toylang-compiled object file calls `Vec::<Point>::new`
by symbol name, but rustc only "mentions" it (doesn't "use" it), the symbol might not
be emitted. Whether this actually happens depends on whether the mentioned→used
promotion logic covers this case.

From the code, mentioned items ARE recursed into and their own dependencies are
collected. The comment says:

> "mentioned" items are only considered internally during collection.

And:

> this is *not* soundness-critical and the contents of this list are *not* a stable
> guarantee.

This means rustc reserves the right to change what `mentioned_items` triggers across
nightlies.

### Tradeoffs

**Advantages:**
- Dramatically simpler MIR. The body is 2 statements (call + return). No function
  pointer constants, no phantom structs, no extra arguments.
- The external Toylang function has its natural signature — no phantom parameter.
- Adding or removing Rust dependencies is a metadata-only change to the body. No
  structural MIR changes needed.
- Cleanly separates "what this function does" (the call stub) from "what Rust
  instantiations it needs" (the mentioned_items list). Good separation of concerns.

**Disadvantages:**
- Mentioned items live in `state.mentioned`, not `state.visited`. The distinction
  between "mentioned" and "used" is subtle. In the current nightly (2025-01-15), the
  collector DOES transitively process mentioned items and their dependencies get
  codegen'd. But this is explicitly documented as not a stable guarantee.
- `mentioned_items` was designed for a specific purpose: ensuring that optimizing away
  function calls doesn't hide const-eval errors. Using it to force monomorphization of
  functions that are only needed by an external linker-level dependency is outside its
  intended purpose. A future nightly could change the semantics in a way that breaks
  this use case.
- The `mentioned_items` field has lifecycle constraints. For `mir_built` bodies,
  `mir_promoted` sets `mentioned_items` automatically. If we set it in `mir_built`,
  it may conflict with the later pass (the "have already been set" panic). We would
  need to verify that setting it in `mir_built` doesn't trigger this, OR set it in
  a later query override.
- Debugging is harder. When a Rust instantiation is missing from the final binary,
  the failure mode is a linker error (undefined symbol). With Approach A, you can
  inspect the MIR and see the function pointer references. With Approach B, you have
  to check the `mentioned_items` metadata, which isn't visible in MIR dumps by default.

---

## Comparison Summary

| Dimension                        | A: Phantom Struct             | B: mentioned_items           |
|----------------------------------|-------------------------------|------------------------------|
| MIR complexity                   | ~10-15 statements per stub    | ~2 statements per stub       |
| Toylang function signature       | Extra phantom pointer arg     | Natural signature            |
| Monomorphization guarantee       | Strong (items are "used")     | Weaker (items are "mentioned", not "used") |
| Stability across nightlies       | Robust (uses normal MIR refs) | Fragile (relies on internal metadata semantics) |
| Intended use case match          | Yes (function refs in MIR)    | Partial (designed for const-eval, not external codegen) |
| mentioned_items lifecycle issue  | N/A                           | May conflict with mir_promoted |
| Debugging / inspectability       | Function refs visible in MIR  | Hidden in metadata           |
| Implementation effort            | Medium (struct construction)  | Low (just set a Vec field)   |
| Separation of concerns           | Mixed (deps encoded as code)  | Clean (deps are metadata)    |

---

## Recommendation

**Use Approach A (Phantom Function Pointer Struct) as the primary mechanism.** The
stronger monomorphization guarantee and robustness across nightlies outweigh the
additional MIR complexity. Verified working: the ReifyFnPointer casts survive all MIR
optimization passes (confirmed via `-Z dump-mir`), and the monomorphization collector
adds the referenced functions to `used_items` (guaranteed codegen).

**Symbol visibility** is a separate problem. The phantom struct ensures monomorphization
(the function IS compiled), but rustc's partitioner may give it internal linkage if all
references are within one CGU. The current solution: **`-C codegen-units=16`** forces
the partitioner to split functions across CGUs, requiring external linkage for cross-CGU
references. See `design-codegen-integration.md` for the full investigation of alternative
approaches (LTO, objcopy, ld -r, etc.) and why they failed.

The combination of phantom struct (for monomorphization) + multiple CGUs (for external
linkage) + CodegenBackend wrapper (for injecting Toylang's .o into the link step) is
the current working approach. It handles both generic and non-generic Toylang functions
uniformly, with no `#[no_mangle]` wrappers needed.

**Keep Approach B in mind as a potential simplification** if a future nightly provides
a more explicit "force-monomorphize this list of items" API, or if `mentioned_items`
semantics are strengthened to guarantee codegen of mentioned items.
