use std::hint::black_box;
use std::time::Instant;

fn main() {
    let iters: i64 = 100_000_000;
    let start = Instant::now();
    let mut sum: i64 = 0;
    for i in 0..iters {
        // Same shape as bench1_*/rust_caller.rs but calls
        // test_helpers::bench_baseline_add (pure Rust) instead of
        // __lang_stubs::add (Sky). Measures the Rust cross-crate call
        // cost as a reference point for Sky's call boundary.
        let i = black_box(i);
        sum = sum.wrapping_add(test_helpers::bench_baseline_add(i as i32, 1) as i64);
    }
    let elapsed = start.elapsed();
    test_helpers::println_i64(black_box(sum));
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
