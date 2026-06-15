# Draft: rust-lang/rust PR — debuginfo struct walker clamp

This file holds the draft PR description + reproducer notes for the
defensive clamp in `build_struct_type_di_node`. Not for upstream; just
working notes.

## Title

debuginfo: clamp struct field walk to layout's field count

(Alternate: "debuginfo: defensive bound on `build_struct_type_di_node`
when layout has fewer fields than source")

## Description (body)

`build_struct_type_di_node` iterates an ADT's source-level `FieldDef`s
and queries `layout.field(cx, i)` / `layout.fields.offset(i)` per field.
When a codegen backend plugin installs a `layout_of` provider that
reports a `FieldsShape::Arbitrary` with fewer offsets than the ADT's
source field count — e.g., an "opaque sized blob" layout where the
plugin wants rustc to see the type as an opaque memory region rather
than indexing into its fields — these indexing calls panic with
`index out of bounds: the len is 0 but the index is 0` (or
analogous), ICE'ing the debuginfo walker.

This commit adds a defensive clamp:

```rust
let visible_field_count = std::cmp::min(
    variant_def.fields.len(),
    struct_type_and_layout.fields.count(),
);
variant_def.fields.iter().take(visible_field_count).enumerate()...
```

The resulting DI node has `min(source_count, layout_count)` fields,
which matches what the plugin asked rustc to see. On unoverridden
layouts (source count and layout count always agree), the clamp is a
no-op.

## Motivation

We hit this in [Sky](https://github.com/…)'s rustc integration, which
overrides `layout_of` to report opaque-sized layouts for Sky-defined
ADTs that should appear opaque to rustc (the host-language frontend
owns the real field layout). The crash reproduces inside `Vec<SkyAdt>`
because the Vec's generic-type-param DI walker recurses into the
inner type's DI node.

Cranelift's backend and miri have similar override-layout patterns
that could in principle hit the same path. The clamp is defensive
for any such plugin, not just Sky-specific.

## Stack trace

```
thread 'rustc' panicked at compiler/rustc_abi/src/lib.rs:1676:66:
index out of bounds: the len is 0 but the index is 0

   3: <rustc_abi::FieldsShape<FieldIdx>>::offset
   4: build_struct_type_di_node closure (mapping over FieldDef iter)
   6: build_type_with_children<build_struct_type_di_node>
   7: build_struct_type_di_node
   8: spanned_type_di_node
   9: build_generic_type_param_di_nodes closure
  11: build_generic_type_param_di_nodes
  12: build_type_with_children<build_struct_type_di_node>  ← Vec<SkyAdt>'s di node
  13: build_struct_type_di_node                            ← Vec<SkyAdt>
```

## Test

The bug only manifests when a `layout_of` provider returns mismatched
field counts. That requires a plugin codegen backend or
`Config::override_queries` invocation — not expressible as a
`tests/ui` or `tests/codegen` test against pure rustc. I've verified
the fix locally against Sky's reproducer: the same fixture that ICE'd
before the patch compiles cleanly with it.

If reviewers want an in-tree regression test, I can sketch one using
`rustc_driver`'s `Config::override_queries` from a test harness, but
the scaffolding cost is real and I'd prefer guidance on whether that's
expected here.

## Risk

- The clamp is purely defensive. Unoverridden compiles have
  `variant_def.fields.len() == struct_type_and_layout.fields.count()`,
  so the `take()` is a no-op.
- The resulting DI node may have fewer fields than the source has, but
  only when a plugin explicitly asked for that. The plugin has made
  the call that those source fields should be invisible to rustc;
  this commit just stops the walker from panicking on that valid
  request.

## Alternative considered

Instead of the clamp, the walker could `delay_span_bug` when source
and layout counts disagree without an explicit opt-in. That's more
heavy-handed and assumes the disagreement is always wrong. The plugin
ecosystem (Sky's opaque-stub design, cranelift's similar patterns) has
legitimate reasons for the mismatch. The silent clamp matches what the
plugin asked for; the bug machinery would block legitimate plugin
usage.

## Filing checklist (before submitting)

- [ ] Confirm the fix against the Sky reproducer post-rebuild
- [ ] Run `./x test compiler/rustc_codegen_llvm` (no specific test
      targets this code path; this confirms no general regression)
- [ ] Check whether similar patterns exist in
      `build_union_type_di_node` (line 1258 in metadata.rs) and
      `build_enum_type_di_node`. The patch may want sibling clamps.
- [ ] Squash to one commit
- [ ] r? @michaelwoerister or current debuginfo maintainer
- [ ] Mention the issue tracker, if Sky has filed one

## Sibling sites audited

- `build_struct_type_di_node` (metadata.rs:~1026): **patched**. The
  source-of-truth site for the bug.
- `build_union_type_di_node` (metadata.rs:~1245): **patched** with the
  same shape. Source iter → `union_ty_and_layout.field(cx, i)` →
  panics if layout has fewer fields than source. Same defensive clamp
  applies, same no-op on unoverridden layouts.
- `build_enum_variant_struct_type_di_node` (metadata/enums/mod.rs:~235):
  no patch needed. Iterates `0..variant_layout.fields.count()` THEN
  indexes `variant_def.fields[i]` — the inverse direction. If a plugin
  reports FEWER layout fields than source (Sky's case), the loop runs
  zero times — no panic. The opposite case (plugin reports MORE
  layout fields than source) would panic at the indexing step, but
  that's a different bug class and not what Sky hits.
- Tuple struct path uses `build_struct_type_di_node` (the patch's
  branch via `tuple_field_name(i)` is on the path).
