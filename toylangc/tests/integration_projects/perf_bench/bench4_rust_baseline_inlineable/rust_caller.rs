// Bench 4 inlineable-Rust baseline driver. Same shape as Bench 4 but
// with #[inline]-attributed helpers; LLVM is free to inline under
// thin LTO. Measures the natural cross-crate boundary cost when
// rustc doesn't artificially block inlining.

use std::time::Instant;

fn main() {
    let n: usize = 10_000_000;
    let start = Instant::now();
    let mut acc: i64 = 0;
    for i in 0..n {
        let big = test_widgets::make_test_large_inlineable(std::hint::black_box(i as i64));
        acc = acc.wrapping_add(test_widgets::first_field_test_large_inlineable(big));
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(acc);
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
