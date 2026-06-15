fn main() {
    let w = __lang_stubs::make_widget(42);
    let copy = std::clone::Clone::clone(&w);
    test_helpers::println_i32(__lang_stubs::id_of(&copy));
}
