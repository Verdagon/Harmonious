# Design: Toylang LLVM Module Integration with rustc's Codegen Pipeline

## The problem

When rustc monomorphizes a generic function like `Vec::<Point>::push`, it gives the
resulting symbol **internal linkage** (`t` in nm) in the object file — only visible
within that compilation unit. A separate Toylang `.o` file cannot reference these symbols.

The phantom struct (ReifyFnPointer casts in MIR) successfully triggers monomorphization —
rustc DOES compile `Vec::<Point>::push`. But then the **partitioner** internalizes the
symbol because all references are within the same CGU.

---

## What we tried

### Approach 1: Fat LTO (`-C lto=fat`)

**Idea:** Merge all LLVM modules (Rust + Toylang) into one; internal linkage irrelevant.

**Result:** Failed. `-C link-arg=foo.o` passes the file to the **system linker** AFTER
rustc's LTO merge. Our module is never part of the merge.

### Approach 2: CodegenBackend wrapper injecting into `join_codegen`

**Idea:** Wrap `CodegenBackend`, intercept `join_codegen`, add Toylang `.o`/`.bc` as
a `CompiledModule` with `bytecode: Some(path)` so Fat LTO picks it up.

**Result:** Failed. LTO runs during `codegen_crate` (inside the coordinator thread).
`join_codegen` runs AFTER LTO is complete. Our module is injected too late.

### Approach 3: Linker-plugin LTO (`-C linker-plugin-lto`)

**Idea:** Defer LTO to the system linker, which sees all inputs including our `.bc`.

**Result:** Failed on macOS. Apple's `ld64` uses Apple's LLVM (`APPLE_1_1500.3.9.4_0`),
which is incompatible with rustc's LLVM 19.1.6:
```
error: Unknown attribute kind (86) (Producer: 'LLVM19.1.6-rust-1.86.0-nightly'
       Reader: 'LLVM APPLE_1_1500.3.9.4_0')
```

### Approach 4: `-Z share-generics=yes`

**Idea:** Force Rust to share generic instantiations with external linkage.

**Result:** No effect. Only changes linkage for generics shared across **crates**, not
within one crate's CGUs.

### Approach 5: `llvm-objcopy --globalize-symbol`

**Idea:** Post-process Rust `.o` files to change specific symbols from local to global.

**Result:** Failed. `--globalize-symbol` is not supported for Mach-O (macOS object
format): `error: option is not supported for MachO`.

### Approach 6: `ld -r` (partial link)

**Idea:** Merge Rust and Toylang `.o` into one relocatable object, resolving internal
references within the merged file.

**Result:** Failed. Local symbols in Mach-O are truly local — `ld -r` combines the
files but cannot resolve references from one file to another file's local symbols.

### Approach 7: Inject before LTO via `ExtraBackendMethods`

**Idea:** Implement `ExtraBackendMethods` to intercept `compile_codegen_unit` and link
Toylang bitcode into the CGU's LLVM module before the coordinator runs LTO.

**Result:** Not attempted — impractical. `ExtraBackendMethods` requires implementing
`WriteBackendMethods` (~15 methods + 6 associated types). The associated types
(`ModuleLlvm`, `ModuleBuffer`, etc.) are `pub(crate)` in `rustc_codegen_llvm` and
cannot be named from an external driver. Would require either an upstream rustc change
or unsafe type-erasure hacks.

---

## What works: `-C codegen-units=16`

**Mechanism:** Forcing multiple codegen units causes the partitioner to place different
functions in different CGUs. When `Vec::<Point>::push` is defined in one CGU but
referenced (via the phantom struct ReifyFnPointer cast) from another CGU, the
partitioner gives it **external linkage** because it must be visible across CGUs.

**Flow:**
```
1. Parse .toylang → ToylangRegistry
2. Mark externally-compiled functions, generate stubs
3. RunCompiler starts (with CodegenBackend wrapper + codegen-units=16)
4. mir_built: phantom ReifyFnPointer stubs → triggers monomorphization
5. after_analysis: generate LLVM IR using tcx.symbol_name() for mangled names
   - Compile .ll → .o via llc
6. Codegen: rustc compiles MIR stubs across 16 CGUs
   - Partitioner sees cross-CGU references → external linkage
7. join_codegen: wrapper injects Toylang .o into CodegenResults
8. Link: system linker resolves all cross-object references
```

**Why it works:**
- Phantom struct creates function references in the MIR stub's CGU
- The referenced functions (Vec::new, Vec::push) are in different CGUs
- Cross-CGU references require external linkage
- Our Toylang `.o` is just another object file that references those same symbols

**Limitations:**
- Relies on the partitioner splitting functions across CGUs. With very small crates,
  16 CGUs might still put everything together. In practice, the phantom struct
  references force cross-CGU usage even in small crates.
- More CGUs means less intra-CGU optimization (no cross-function inlining within
  rustc's own codegen, unless LTO is also enabled).
- Not architecturally "pure" — we're using a side effect of partitioning rather than
  a direct mechanism for controlling symbol linkage.

---

## How rustc's codegen pipeline works

### The flow

```
codegen_crate()
  │
  ├─ Partition mono items into Codegen Units (CGUs)
  │   └─ internalize_symbols(): items used in only 1 CGU → internal linkage
  │
  ├─ For each CGU:
  │   ├─ compile_codegen_unit(tcx, cgu_name)
  │   │   └─ Lower MIR → LLVM IR into a ModuleLlvm
  │   │
  │   └─ submit_codegened_module_to_llvm()
  │       └─ Sends ModuleCodegen<ModuleLlvm> to coordinator thread
  │
  ├─ Coordinator thread:
  │   ├─ WorkItem::Optimize(module) → LLVM optimization passes
  │   ├─ If LTO: WorkItem::LTO(module) → Fat/Thin LTO merging
  │   └─ Emit .o / .bc files → CompiledModule
  │
  └─ join_codegen() → CodegenResults { modules: Vec<CompiledModule> }
         │                    ↑ TOO LATE to inject for LTO
         └─ link() → final binary
              ↑ WHERE our .o gets linked (as a regular object)
```

### Key types

| Type | Location | Role |
|------|----------|------|
| `CodegenBackend` | `rustc_codegen_ssa/src/traits/backend.rs` | Main trait; wrappable from external driver |
| `ExtraBackendMethods` | same file | `compile_codegen_unit` etc.; NOT wrappable (pub(crate) types) |
| `WriteBackendMethods` | `rustc_codegen_ssa/src/traits/write.rs` | ~15 methods + 6 assoc types; NOT wrappable |
| `ModuleLlvm` | `rustc_codegen_llvm/src/lib.rs:401` | `pub(crate)` — owns LLVM Context + Module |
| `CompiledModule` | `rustc_codegen_ssa/src/lib.rs:105` | Paths to `.o`, `.bc` on disk — this IS accessible |
| `CodegenResults` | `rustc_codegen_ssa/src/lib.rs:205` | `modules: Vec<CompiledModule>` — injectable in `join_codegen` |

### Why `ExtraBackendMethods` is hard to wrap

`ExtraBackendMethods` extends `WriteBackendMethods` which has associated types:
```rust
type Module: Send + Sync;        // = ModuleLlvm (pub(crate))
type TargetMachine;               // = OwnedTargetMachine (pub(crate))
type ModuleBuffer: ModuleBufferMethods;  // = back::lto::ModuleBuffer (pub(crate))
type ThinData: Send + Sync;
type ThinBuffer: ThinBufferMethods;
```

These types are `pub(crate)` in `rustc_codegen_llvm`. An external driver cannot name
them, so it cannot implement `WriteBackendMethods` as a delegating wrapper. This
effectively blocks injection into the codegen pipeline before LTO.

### The monomorphization collector and `ReifyFnPointer`

Confirmed in `rustc_monomorphize/src/collector.rs` lines 709-717:
```rust
CastKind::PointerCoercion(PointerCoercion::ReifyFnPointer, _) => {
    let fn_ty = operand.ty(self.body, self.tcx);
    visit_fn_use(self.tcx, fn_ty, false, span, self.used_items);
}
```

`ReifyFnPointer` casts add the referenced function to `used_items` (not just
`mentioned_items`). This guarantees the function is codegen'd. The phantom struct
pattern is robust — the casts survive all MIR optimization passes (verified via
`-Z dump-mir`).

---

## Future options for proper LTO integration

1. **Upstream rustc change**: Make `ModuleLlvm`, `Linker`, and `ModuleBuffer` public.
   This would allow wrapping `ExtraBackendMethods` and injecting bitcode into CGU
   modules before LTO. Small, reasonable contribution.

2. **rust-lld on macOS**: When rust-lld's Mach-O support matures, linker-plugin LTO
   will work without LLVM version mismatch. This is the simplest long-term path.

3. **Inject into coordinator thread**: Submit a `WorkItem` to the coordinator during
   `codegen_crate` via the `shared_emitter` or similar channel. Would require
   understanding the coordinator's message protocol. Complex but no upstream changes.

4. **Use `-C lto=fat` + `codegen-units=16`**: The partitioner externalizes cross-CGU
   symbols. Fat LTO then merges all CGUs + our module. BUT our module is still not
   part of the merge (it's a link-arg). Only helps if we also solve the "too late for
   LTO" timing issue.
