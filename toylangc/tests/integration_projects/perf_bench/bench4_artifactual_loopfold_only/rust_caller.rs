// Bench 4 driver. Pass LargeStruct by value 10M times across the
// Sky/Rust cross-crate boundary; sum the returned i64 to defeat DCE.

use std::time::Instant;

fn main() {
    let n: usize = 10_000_000;
    let start = Instant::now();
    let mut acc: i64 = 0;
    for i in 0..n {
        let big = unsafe { __lang_stubs::make_large(std::hint::black_box(i as i64)) };
        acc = acc.wrapping_add(unsafe { __lang_stubs::first_field(big) });
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(acc);
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
