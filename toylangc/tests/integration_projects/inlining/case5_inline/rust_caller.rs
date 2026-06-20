#[inline]
fn wrap_store(x: i32) -> usize {
    __lang_stubs::store_in_vec::<i32>(x)
}

fn main() {
    let n = wrap_store(7);
    test_helpers::println_usize(n);
}
