use std::hint::black_box;
use std::time::Instant;

fn main() {
    let iters: i64 = 100_000_000;
    let start = Instant::now();
    let mut sum: i64 = 0;
    for i in 0..iters {
        // black_box(i) forces LLVM to treat i as opaque; the call to
        // add() can still be inlined, but the loop's accumulator value
        // can't be reduced to a compile-time constant (which is what
        // the first round of benches measured — Bench 1 thin = 0us
        // because LLVM constant-folded the entire 100M-iter loop).
        let i = black_box(i);
        sum = sum.wrapping_add(__lang_stubs::add(i as i32, 1) as i64);
    }
    let elapsed = start.elapsed();
    // black_box(sum) defeats any final DCE that would notice the sum
    // is never observed.
    test_helpers::println_i64(black_box(sum));
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
