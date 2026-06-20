struct LocalThing { value: i32 }

#[inline]
fn wrap_identity(t: LocalThing) -> LocalThing {
    __lang_stubs::identity::<LocalThing>(t)
}

fn main() {
    let t = LocalThing { value: 42 };
    let r = wrap_identity(t);
    test_helpers::println_i32(r.value);
}
