//! Rust generic intermediaries for the seven-case taxonomy fixtures
//! (architecture §2.6, §2.7).
//!
//! `test_helpers` carries only `#[no_mangle] extern "C"` non-generic
//! functions because toylang calls into it via unmangled symbols. This
//! crate is the architectural complement: ordinary `pub fn foo<T>(...)`
//! Rust generics that exercise the path the architecture's hard cases
//! depend on — rustc walking a Rust generic body, substituting T, seeing
//! a trait-method call, and dispatching to a Sky-defined impl on the
//! consumer's type.
//!
//! No `extern "C"`, no `#[no_mangle]`. Standard Rust generics, dispatched
//! via the Rust-mangled symbol the consumer's `symbol_name` override
//! routes through.

/// Architectural Case 4 intermediary. The Sky caller invokes this with a
/// Sky-defined T whose `Clone` impl lives in Sky source; rustc walks
/// `duplicate<T>`'s body, substitutes T to that Sky type, queues
/// `<T as Clone>::clone`, fires Sky's `per_instance_mir` override at the
/// substituted impl method.
pub fn duplicate<T: Clone>(x: &T) -> T {
    x.clone()
}

/// Architectural Case 5 intermediary — a Rust generic that stores its
/// argument in a `Vec` and returns the resulting length. Layered over
/// Sky's generic middle: `store_in_vec<T>(x: T) -> usize` (Sky) calls
/// this from Sky source. Rust top side just observes the usize.
pub fn make_vec_of_three<T: Copy>(x: T) -> Vec<T> {
    let mut v = Vec::new();
    v.push(x);
    v.push(x);
    v.push(x);
    v
}
