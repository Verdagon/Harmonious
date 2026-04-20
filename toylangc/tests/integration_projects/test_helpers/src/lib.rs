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
pub extern "C" fn println_i64(x: i64) {
    println!("{}", x);
}

#[no_mangle]
pub extern "C" fn println_u32(x: u32) {
    println!("{}", x);
}

#[no_mangle]
pub extern "C" fn println_u8(x: u8) {
    println!("{}", x);
}

/// Print "ok" — used by tests that just need to verify the binary ran to
/// completion (compile-or-not tests where the value of main isn't itself
/// the assertion target).
#[no_mangle]
pub extern "C" fn println_ok() {
    println!("ok");
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

/// Option/Result extern helpers. Toylang's `fn make_some_i32(x: i32) ->
/// Option<i32>;` decl becomes an `extern "C"` declaration in stub_gen's
/// emitted rlib; match that ABI here with `#[no_mangle] pub extern "C"`
/// plus the `improper_ctypes_definitions` allow. rustc's ABI lowering
/// for `Option<i32>` / `Result<i32, i32>` / `Option<u8>` happens to
/// match what toylang emits at the call site (sret + scalar return
/// register choice lines up) so the link succeeds and the round-trip
/// works. Non-FFI-safe by rustc's strict definition but concrete ABI
/// matches under our controlled toolchain.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn make_some_i32(x: i32) -> Option<i32> {
    Some(x)
}

#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn make_ok_i32(x: i32) -> Result<i32, i32> {
    Ok(x)
}

#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn get_byte() -> Option<u8> {
    Some(7)
}

/// Return length of byte slice. Used by tests that exercise the `&[u8]`
/// ScalarPair ABI — toylang passes `b"..."` and the test asserts the
/// length round-trips. toylang's `fn check_bytes(data: &[u8]) -> i32`
/// decl gets emitted as `extern "C"` by stub_gen, so match that ABI
/// here; rustc will warn `improper_ctypes` on `&[u8]` + extern "C"
/// but the ABI lowering happens to match toylang's emitted call.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn check_bytes(data: &[u8]) -> i32 {
    data.len() as i32
}

/// Return length of `&str`. Same ABI situation as `check_bytes` — both
/// `&str` and `&[u8]` are ScalarPair { ptr, len } under extern "C" and
/// rustc lowers them compatibly with toylang's emitted call sites.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn check_str(data: &str) -> i32 {
    data.len() as i32
}

/// Roguelike fixture helpers. Move-list semantics mirror the direct-mode
/// test fixture: canned 10-move sequence that walks the player into
/// ghosts on grid positions we assert against via stdout.
static MOVES: &[i32] = &[
    0, 0, 0,
    3, 3, 3, 3,
    1, 1,
];

#[no_mangle]
pub extern "C" fn board_is_wall(row: i32, col: i32) -> bool {
    row <= 0 || row >= 9 || col <= 0 || col >= 9
}

#[no_mangle]
pub extern "C" fn get_move(i: i32) -> i32 {
    MOVES[i as usize]
}

#[no_mangle]
pub extern "C" fn num_moves() -> i32 {
    MOVES.len() as i32
}
