// Bench 4 pure-Rust baseline driver. Same shape as Bench 4 but with
// LargeStruct + helpers in the test_widgets sibling crate — no Sky
// emission boundary in the inner loop.

use std::time::Instant;

fn main() {
    let n: usize = 10_000_000;
    let start = Instant::now();
    let mut acc: i64 = 0;
    for i in 0..n {
        let big = test_widgets::make_test_large(std::hint::black_box(i as i64));
        acc = acc.wrapping_add(test_widgets::first_field_test_large(big));
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(acc);
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
