use std::hint::black_box;
use std::time::Instant;

fn main() {
    let iters: i64 = 10_000_000;
    let start = Instant::now();
    let mut sum: i64 = 0;
    for i in 0..iters {
        // black_box: see bench1_*/rust_caller.rs for rationale.
        let i = black_box(i);
        sum = sum.wrapping_add(__lang_stubs::work(i as i32, 3) as i64);
    }
    let elapsed = start.elapsed();
    test_helpers::println_i64(black_box(sum));
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
