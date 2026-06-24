//! Generics-heavy pure-Rust pass-through fixture.
//!
//! Exercises rustc's mono collector + share_generics paths via many
//! concrete instantiations of generic functions and trait impls. If
//! Sky's `cross_crate_inlinable` or `codegen_fn_attrs` overrides
//! leaked into pure-Rust compiles (no marker present), this fixture
//! would surface it as either compile errors, link errors, or
//! runtime-output drift.

use std::collections::HashMap;

trait Combine<T> {
    fn combine(&self, other: T) -> T;
}

impl Combine<i32> for i32 {
    fn combine(&self, other: i32) -> i32 {
        self.wrapping_add(other)
    }
}

impl Combine<i64> for i64 {
    fn combine(&self, other: i64) -> i64 {
        self.wrapping_mul(other)
    }
}

fn fold_with<T, F>(items: &[T], init: T, mut f: F) -> T
where
    T: Copy,
    F: FnMut(T, T) -> T,
{
    let mut acc = init;
    for &item in items {
        acc = f(acc, item);
    }
    acc
}

#[derive(Debug, Clone)]
struct Pair<A, B> {
    first: A,
    second: B,
}

impl<A: Clone, B: Clone> Pair<A, B> {
    fn swap(self) -> Pair<B, A> {
        Pair { first: self.second, second: self.first }
    }
}

fn main() {
    // Vec<i32> mono of generic combinators.
    let xs_i32: Vec<i32> = (0..50).collect();
    let sum_i32 = fold_with(&xs_i32, 0i32, |a, b| a.wrapping_add(b));
    let combined_i32 = 5i32.combine(7);

    // Vec<i64> mono.
    let xs_i64: Vec<i64> = (1..=10).collect();
    let product_i64 = fold_with(&xs_i64, 1i64, |a, b| a.wrapping_mul(b));
    let combined_i64 = 6i64.combine(7);

    // Pair<i32, String> exercises generic struct with non-Copy field.
    let p = Pair { first: 42i32, second: "world".to_string() };
    let swapped = p.swap();

    // HashMap<i32, String> exercises a heavy stdlib generic.
    let mut h: HashMap<i32, String> = HashMap::new();
    for i in 0..5 {
        h.insert(i, format!("v{}", i));
    }
    let h_len = h.len();

    println!("sum_i32={} combined_i32={}", sum_i32, combined_i32);
    println!("product_i64={} combined_i64={}", product_i64, combined_i64);
    println!("swapped.first={} swapped.second={}", swapped.first, swapped.second);
    println!("h_len={}", h_len);
}
