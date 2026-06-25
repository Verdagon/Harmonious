// Regression driver: Rust caller dereferences each field accessor's
// returned `&bool`. Pre-fix output was all `false`; post-fix matches
// the values Sky stored (true false true false true).

fn main() {
    let w = unsafe { __lang_stubs::make_w() };
    println!("{} {} {} {} {}", *w.a(), *w.b(), *w.c(), *w.d(), *w.e());
}
