struct LocalThing { value: i32 }

fn main() {
    let t = LocalThing { value: 42 };
    let r = __lang_stubs::identity::<LocalThing>(t);
    test_helpers::println_i32(r.value);
}
