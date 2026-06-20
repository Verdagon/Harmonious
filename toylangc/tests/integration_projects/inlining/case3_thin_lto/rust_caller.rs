#[derive(Clone)]
struct MyCounter { count: i32 }

fn main() {
    let c = MyCounter { count: 42 };
    let copy = __lang_stubs::clone_it::<MyCounter>(&c);
    test_helpers::println_i32(copy.count);
}
