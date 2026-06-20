#[inline]
fn wrap_add_one(x: i32) -> i32 {
    __lang_stubs::add_one(x)
}

fn main() {
    let r = wrap_add_one(41);
    test_helpers::println_i32(r);
}
