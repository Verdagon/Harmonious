// Architectural Case 5: Rust top → Sky generic middle → different Rust.
// The Rust top originates the concrete T (here `i32`, but could equally be
// a user-defined struct). Sky's `store_in_vec<T>` is GENERIC — when Rust
// calls it with T=i32 the Approach-A per_instance_mir override fires with
// non-empty `instance.args`, substitutes T, and the body's `Vec::new<T,
// Global>` becomes `Vec::<i32, Global>::new` queued for rustc's Rust-
// generic monomorphization. This is the "1b layered over 2" hard case
// the architecture's §2.7 worked example describes.
fn main() {
    let n = __lang_stubs::store_in_vec::<i32>(7);
    test_helpers::println_usize(n);
}
