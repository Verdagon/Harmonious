# Generate Compile Mutex Lock (GCMLZ)

`call_generate_and_compile` holds a mutex on mutable consumer state while the
consumer's LLVM codegen runs. During codegen, tcx queries (symbol_name,
layout_of, fn_abi_of_instance) may trigger custom query providers. Those
providers must not lock the same mutex, or the single-threaded process deadlocks.

The fix: immutable config (callbacks, vtable, default query providers) lives in
lock-free `OnceLock` statics (`CONFIG`, `DEFAULT_LAYOUT_OF`, etc.), separate
from the mutable state mutex (`MUTABLE_STATE`). Query providers read config
without locking; only callbacks that need `&mut consumer_state` lock
`MUTABLE_STATE`.

## Where

- `rustc-lang-facade/src/lib.rs` — `CONFIG` (OnceLock), `MUTABLE_STATE` (Mutex),
  `DEFAULT_*` (OnceLock) statics; `call_generate_and_compile` locks only
  `MUTABLE_STATE`; `is_consumer_type`/`is_consumer_fn`/`default_*` read from
  OnceLocks
- `rustc-lang-facade/src/codegen_wrapper.rs` — `codegen_crate` calls
  `call_generate_and_compile` after `inner.codegen_crate` completes
- `rustc-lang-facade/src/queries/symbol_name.rs` — `lang_symbol_name` calls
  `is_consumer_fn` (CONFIG, no lock) and `default_symbol_name` (OnceLock, no
  lock) for non-consumer functions
- `rustc-lang-facade/src/queries/layout.rs` — `toy_layout_of` calls
  `is_consumer_type` (CONFIG, no lock) and `default_layout_of` (OnceLock, no
  lock) for non-consumer types
- `rustc-lang-facade/src/queries/drop_glue.rs` — `toy_mir_shims` same pattern
- `rustc-lang-facade/src/queries/per_instance.rs` — `lang_per_instance_mir`
  same pattern

## Cross-cutting effect

Any new query provider or any new tcx query made during `generate_and_compile`
must not lock `MUTABLE_STATE`. If it does, the process deadlocks silently (0%
CPU, hangs forever). This is easy to trigger accidentally: calling a function
like `default_symbol_name()` that previously locked a shared `GLOBALS` mutex
would deadlock when called during codegen.

The deadlock was latent for all existing tests because their tcx queries
(symbol_name for Vec::push, layout_of for Vec, etc.) were cached during
`inner.codegen_crate`. The first uncached query during `generate_and_compile`
was `tcx.symbol_name(stdout)` — `stdout` is a use-imported free function whose
symbol name was never computed during `inner.codegen_crate`.

Residual risk: if a query provider needs `&mut consumer_state` for an uncached
consumer item during `generate_and_compile`, it will deadlock. This is currently
prevented by the fact that all consumer items are discovered and cached during
`inner.codegen_crate` (monomorphization phase).

## Why it exists

The facade needs global mutable state for consumer callbacks (`monomorphize_fn`,
etc.) and also needs to intercept rustc's query providers (which are plain
function pointers with no way to pass state). The global mutex serializes all
access. But `generate_and_compile` runs for the entire duration of consumer
codegen (building LLVM IR, running llc), and any tcx query during that time
passes through the custom query providers. A single non-reentrant mutex covering
both immutable config and mutable state makes this impossible — hence the split.
