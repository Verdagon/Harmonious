// Bench 4b pure-Rust inlineable baseline driver. Same shape as
// bench4b_blackbox_sky but with #[inline] helpers in test_widgets.

use std::time::Instant;

fn main() {
    let n: usize = 10_000_000;
    let start = Instant::now();
    let mut acc: i64 = 0;
    for i in 0..n {
        let big = test_widgets::make_test_large_inlineable(std::hint::black_box(i as i64));
        let big = std::hint::black_box(big);
        acc = acc.wrapping_add(test_widgets::first_field_test_large_inlineable(big));
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(acc);
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
