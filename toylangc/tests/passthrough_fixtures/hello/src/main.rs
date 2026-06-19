//! Passthrough corpus fixture. Pure Rust — no `__SKY_STUBS_MARKER` anywhere.
//! Exercises a few stdlib monomorphisations (println formatting, Vec,
//! iterator combinators, integer arithmetic) so the codegen path actually
//! touches non-trivial Rust generics.

fn main() {
    println!("hello, passthrough");
    let v: Vec<i32> = (0..10).collect();
    let sum: i32 = v.iter().sum();
    let product: i64 = (1..=5i64).product();
    println!("sum={} product={}", sum, product);
    let mapped: Vec<String> = v.iter().map(|n| format!("[{}]", n)).collect();
    println!("mapped={}", mapped.join(""));
}
