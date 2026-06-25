//! B26 drift-observation fence — `InstanceKind` + `MonoItem` variant coverage.
//!
//! Per arch §25.2 B26 + handoff Decision 15: Sky relies on rustc's
//! `MonoItem::Fn` being the only variant carrying drop semantics during
//! cascade discovery (`callbacks_impl::collect_consumer_trait_impl_instances`),
//! and on `InstanceKind::DropGlue` being the only variant rustc uses for
//! cross-language drop dispatch through the standard path.
//!
//! `AsyncDropGlue` already exists as an experimental MIR transform; if it
//! gets promoted to mono-item level (or a new `OwnedDropGlue` / similar
//! variant lands), Sky's discovery walks need explicit handling. Without
//! this fence, the day rustc bumps and adds a new variant, Sky's cascade
//! would silently skip those instances — and the failure would manifest
//! as memory leaks / use-after-free at runtime, not at build time.
//!
//! The fence is a compile-time exhaustive match: if rustc adds a new
//! variant to `MonoItem` or `InstanceKind`, this test stops compiling
//! with a `non_exhaustive_omitted_patterns` warning that's promoted to a
//! deny. Future maintainers MUST decide whether the new variant carries
//! drop semantics and update Sky's cascade discovery accordingly.
//!
//! Reaction if this fence fires:
//!   1. Read the new variant's docstring in rustc's source.
//!   2. If it carries drop semantics (or any other Sky-relevant
//!      cascade-discoverable behavior), extend
//!      `callbacks_impl::collect_consumer_trait_impl_instances` to walk
//!      the new variant alongside `MonoItem::Fn`.
//!   3. Add the new variant to the match arms below.
//!   4. Update arch §25.2 B26 with the new variant's name + impact.

#![feature(rustc_private)]

// `MonoItem` and `InstanceKind` are not `#[non_exhaustive]` enums, so
// vanilla pattern-match exhaustiveness checking (E0004) fires if a new
// variant is added without an arm. No `#[deny(...)]` needed.

extern crate rustc_driver;
extern crate rustc_middle;

/// Compile-time exhaustive-match fence over `MonoItem` variants. If
/// rustc adds a new variant (e.g. `MonoItem::AsyncDropGlue`), this
/// match becomes non-exhaustive and the `deny(non_exhaustive_omitted_patterns)`
/// attribute fires at build time. The handler maps each variant to a
/// human-readable label so changes show up as concrete diffs.
#[allow(dead_code)]
fn mono_item_variant_label(item: &rustc_middle::mir::mono::MonoItem<'_>) -> &'static str {
    use rustc_middle::mir::mono::MonoItem;
    match item {
        // The variant Sky's cascade currently walks. Any new MonoItem
        // variant that carries drop semantics OR carries an Instance
        // whose def_id matches a consumer trait-impl method must be
        // added to the discovery walk in `callbacks_impl`.
        MonoItem::Fn(_) => "Fn",
        MonoItem::Static(_) => "Static",
        MonoItem::GlobalAsm(_) => "GlobalAsm",
    }
}

/// Compile-time exhaustive-match fence over `InstanceKind` variants.
/// `DropGlue` is the variant Sky depends on for drop dispatch through
/// rustc's standard path. If rustc adds `AsyncDropGlue` or another
/// drop-flavored variant (probable — `AsyncDropGlue` already exists in
/// experimental MIR transforms), Sky's discovery needs to handle it.
#[allow(dead_code)]
fn instance_kind_variant_label(kind: &rustc_middle::ty::InstanceKind<'_>) -> &'static str {
    use rustc_middle::ty::InstanceKind;
    match kind {
        InstanceKind::Item(_) => "Item",
        InstanceKind::Intrinsic(_) => "Intrinsic",
        InstanceKind::VTableShim(_) => "VTableShim",
        InstanceKind::ReifyShim(_, _) => "ReifyShim",
        InstanceKind::FnPtrShim(_, _) => "FnPtrShim",
        InstanceKind::Virtual(_, _) => "Virtual",
        InstanceKind::ClosureOnceShim { .. } => "ClosureOnceShim",
        InstanceKind::DropGlue(_, _) => "DropGlue",
        InstanceKind::CloneShim(_, _) => "CloneShim",
        InstanceKind::ThreadLocalShim(_) => "ThreadLocalShim",
        InstanceKind::ConstructCoroutineInClosureShim { .. } => "ConstructCoroutineInClosureShim",
        InstanceKind::FnPtrAddrShim(_, _) => "FnPtrAddrShim",
        InstanceKind::AsyncDropGlue(_, _) => "AsyncDropGlue",
        InstanceKind::AsyncDropGlueCtorShim(_, _) => "AsyncDropGlueCtorShim",
        InstanceKind::FutureDropPollShim(_, _, _) => "FutureDropPollShim",
    }
}

#[test]
fn mono_item_variants_remain_covered() {
    // The test passes as long as the function compiles. If a new
    // variant is added to `MonoItem`, the `deny(non_exhaustive_omitted_patterns)`
    // at the crate root fires and this file fails to build.
    //
    // Print the label set for observability so a regression-fixer can see
    // what's expected vs missing. Sky's cascade walks `Fn` today; if a new
    // drop-flavored variant lands, it likely needs handling alongside `Fn`.
    eprintln!("Sky-tracked MonoItem variants: Fn (consumer-discovery), Static, GlobalAsm");
}

#[test]
fn instance_kind_variants_remain_covered() {
    // Same fence pattern for `InstanceKind`. The list here is the
    // post-2026-06-25 nightly's set. If rustc adds a new variant the
    // function stops compiling and a maintainer must decide whether
    // it's drop-flavored / cascade-relevant.
    //
    // **DropGlue** is the Sky-load-bearing variant. **AsyncDropGlue**
    // and **AsyncDropGlueCtorShim** are emergent (already present in the
    // pinned nightly); Sky doesn't yet handle them because toylang has
    // no async-drop surface. When async-drop lands in Sky (Phase M),
    // these need explicit handling in `collect_consumer_trait_impl_instances`.
    eprintln!(
        "Sky-tracked InstanceKind variants: DropGlue (load-bearing), \
         AsyncDropGlue (emergent, TODO Phase M), AsyncDropGlueCtorShim (emergent)"
    );
}
