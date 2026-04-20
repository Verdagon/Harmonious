//! Shared helpers for toylangc integration tests.
//!
//! Stage 5c.1: integration tests no longer ship arbitrary inline Rust
//! fixtures via FileLoader; they're real toylang projects compiled by
//! `toylangc build`. Toylang's body-less `fn foo();` extern declarations
//! are resolved at link time against this crate's `pub fn foo` definitions.
//!
//! Each helper is `#[no_mangle] pub extern "C"` so toylang's emitted code
//! can call them via unmangled symbol — toylang does not consume rustc's
//! per-crate name mangling. Keep ABI primitive (i32 / bool / usize / `&str`
//! / `&[u8]`); anything richer crosses the toylang/Rust boundary in a way
//! that needs a wrapper in `__lang_stubs` rather than a helper here.

#[no_mangle]
pub extern "C" fn println_int(x: i32) {
    println!("{}", x);
}

#[no_mangle]
pub extern "C" fn println_i32(x: i32) {
    println!("{}", x);
}

#[no_mangle]
pub extern "C" fn println_bool(x: bool) {
    println!("{}", x);
}

#[no_mangle]
pub extern "C" fn println_usize(x: usize) {
    println!("{}", x);
}

#[no_mangle]
pub extern "C" fn print_int(x: i32) {
    print!("{} ", x);
}

#[no_mangle]
pub extern "C" fn add(x: i32, y: i32) -> i32 {
    x + y
}

#[no_mangle]
pub extern "C" fn add_one(x: i32) -> i32 {
    x + 1
}

#[no_mangle]
pub extern "C" fn add_ten(x: i32) -> i32 {
    x + 10
}

#[no_mangle]
pub extern "C" fn double(x: i32) -> i32 {
    x * 2
}

#[no_mangle]
pub extern "C" fn identity(x: i32) -> i32 {
    x
}

pub fn make_some_i32(x: i32) -> Option<i32> {
    Some(x)
}

pub fn make_ok_i32(x: i32) -> Result<i32, i32> {
    Ok(x)
}

pub fn get_byte() -> Option<u8> {
    Some(7)
}
