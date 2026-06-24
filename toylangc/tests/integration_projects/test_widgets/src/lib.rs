//! Pure-Rust Widget types for Bench 3's apples-to-apples baselines.
//!
//! These mirror Sky's bench3 Widget — a 4-byte struct with an empty Drop
//! impl — but defined entirely in Rust source. The bench fixtures under
//! `perf_bench/bench3_rust_baseline_*` measure the cross-crate Drop chain
//! when both ends are Rust, providing the reference column for Sky's
//! 26.5× LTO ratio: does pure Rust show the same ratio under matching
//! cross-crate conditions?
//!
//! Two variants:
//! - `Widget`: standard `impl Drop` with empty body. LLVM is free to
//!   inline + eliminate the body under LTO.
//! - `WidgetNoInline`: same shape but the Drop impl carries
//!   `#[inline(never)]`, forcing a real function call per element. This
//!   establishes the floor — what does the chain cost when the inliner
//!   literally can't eliminate the body?
//!
//! `make_test_widget` + `make_test_widget_no_inline` are the Vec-build
//! helpers, both `#[inline(never)]` to match the structural shape of
//! `__lang_stubs::make_widget` (which is a cross-crate extern call from
//! the user_bin's view). Returning a struct by-value across `extern "C"`
//! is non-FFI-safe by rustc's strict definition but round-trips under
//! our controlled toolchain — same pattern as `test_helpers::make_some_i32`.

pub struct Widget {
    pub id: i32,
}

impl Drop for Widget {
    fn drop(&mut self) {}
}

pub struct WidgetNoInline {
    pub id: i32,
}

impl Drop for WidgetNoInline {
    #[inline(never)]
    fn drop(&mut self) {}
}

#[allow(improper_ctypes_definitions)]
#[inline(never)]
#[no_mangle]
pub extern "C" fn make_test_widget(id: i32) -> Widget {
    Widget { id }
}

#[allow(improper_ctypes_definitions)]
#[inline(never)]
#[no_mangle]
pub extern "C" fn make_test_widget_no_inline(id: i32) -> WidgetNoInline {
    WidgetNoInline { id }
}
