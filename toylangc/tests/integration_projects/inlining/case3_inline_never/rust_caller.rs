#[derive(Clone)]
struct MyCounter { count: i32 }

#[inline(never)]
fn wrap_clone_it(c: &MyCounter) -> MyCounter {
    __lang_stubs::clone_it::<MyCounter>(c)
}

fn main() {
    let c = MyCounter { count: 42 };
    let copy = wrap_clone_it(&c);
    test_helpers::println_i32(copy.count);
}
