# Generate Compile Mutex Lock (GCMLZ)

`MUTABLE_STATE` is a `Mutex<Box<dyn Any>>` wrapping the consumer's mutable
state object. Four stateful callbacks lock it while running:
`collect_generic_rust_deps`, `after_rust_analysis`, `on_sky_lib_loaded`,
`consumer_emit_modules`.

Historically the lock would deadlock when a tcx query fired during a
held-mutex callback, because some query providers also locked. As of
Tier 3 #7 + #9 + #4-deeper, that entire scenario class is dissolved:

- Predicates (`is_consumer_type` / `is_consumer_fn`) read from
  `SkyUniverse` (`RwLock`, lock-free in practice). They never touch
  `MUTABLE_STATE`.
- `lang_layout_of`'s callback (`monomorphize_type`) is stateless — its
  trampoline takes no `&mut state`. Dispatched without locking
  `MUTABLE_STATE`.
- `lang_symbol_name`'s callback (`consumer_symbol_for_callback_name`)
  is stateless — same pattern.
- `lang_per_instance_mir`'s callback (`collect_generic_rust_deps`)
  takes `&mut state` AND fires during rustc's mono walk (which uses
  rayon worker threads, so concurrent fires are possible). It locks
  `MUTABLE_STATE` per call.

The thread-local fat-pointer bypass that Session 5 added for
`symbol_name`-during-codegen re-entrance retired with Tier 3 #9 — no
re-entrance path remains, so no bypass needed.

## What `MUTABLE_STATE` still serializes

Four callbacks share `&mut Box<dyn Any>` consumer state:

| Callback | When it fires | Concurrent? |
|---|---|---|
| `collect_generic_rust_deps` | `lang_per_instance_mir` during mono walk | Yes — rayon workers |
| `after_rust_analysis` | After rustc typechecks user-bin source | Main thread, once |
| `on_sky_lib_loaded` | For each upstream Sky lib's sidecar | Main thread, once per lib |
| `consumer_emit_modules` | `extra_modules_hook` during `codegen_crate` | Main thread, once |

`collect_generic_rust_deps` is the lone reason the mutex isn't an
`UnsafeCell` or a single-threaded `RefCell`. The other three are
inherently main-thread-serialised by rustc's compile lifecycle.

## Where

- `rustc-lang-facade/src/lib.rs` — `MUTABLE_STATE` (Mutex), `CONFIG`
  (OnceLock for immutable callbacks + vtable), `DEFAULT_*` (OnceLock
  for upstream query providers). `call_*` helpers for the four
  stateful callbacks lock `MUTABLE_STATE`; the predicate helpers
  (`is_consumer_type` / `is_consumer_fn`) and stateless trampolines
  (`call_monomorphize_type`, `call_consumer_symbol_for_callback_name`)
  do not.
- `rustc-lang-facade/src/queries/{symbol_name,layout,drop_glue,per_instance,partition,upstream_monomorphization}.rs`
  — every query override reads CONFIG / DEFAULT_* (OnceLock,
  lock-free) and (where applicable) consults `SkyUniverse` (RwLock
  read, lock-free).
- `rustc-lang-facade/src/extra_modules_hook.rs` — Phase 4.5 / Path B's
  inline-codegen path. Calls `consumer_emit_modules` which locks
  `MUTABLE_STATE` for the duration. No re-entrance through query
  providers because the providers are lock-free.

## Cross-cutting effect

Adding a new query provider that locks `MUTABLE_STATE` would
re-introduce deadlock risk under `collect_generic_rust_deps` (the
worker-thread caller). The default-safe pattern: read from
`SkyUniverse` for content lookups, use `CONFIG` for immutable callback
dispatch, never touch `MUTABLE_STATE` from a query provider.

Adding a new stateful callback (`&mut consumer_state` in the
signature) means picking the right vtable slot and locking via the
`call_*` helper. The lock contract is by convention; there's no
type-level enforcement now that `PredicateVtable` retired.

## What this arcana used to say

Earlier versions described:

- A two-vtable split (`PredicateVtable` + `StatefulVtable`) where the
  predicate vtable's signatures literally couldn't accept `&mut
  state`, enforcing lock-freedom at the type level. Tier 3 #7.4 moved
  predicates to `SkyUniverse` and retired the trait + vtable.
- A `notify_concrete_entry_point` callback that fired from
  `symbol_name` and locked `MUTABLE_STATE`, requiring Session 5's
  thread-local fat-pointer bypass to dodge the re-entrant lock during
  `generate_and_compile`. Tier 3 #9 made the symbol mangler stateless;
  the callback + bypass retired together.
- `generate_and_compile` as the long-running stateful callback that
  held `MUTABLE_STATE` for all of consumer codegen. Phase 4.5 / Path B
  (Session 15) replaced it with `consumer_emit_modules` via the
  `extra_modules_hook` fork patch. The lock-during-codegen pattern is
  unchanged in shape but the codegen is now bitcode emission only —
  no more `llc` shell-out, no in-house LLVM context.

## Why it exists

The facade owns consumer mutable state on behalf of the rustc plugin.
Rustc's query providers are plain `fn` pointers with no captured
state. The state has to live in a `static`; that means `Send + Sync`;
that means some form of synchronisation. `Mutex<Box<dyn Any>>` is the
simplest correct choice. The two-vtable / lock-free-predicates work
removed the deadlock failure modes; the mutex now serves only
inter-callback serialisation for the four callbacks that actually
mutate the state.
