// Bench 4b driver. black_box on the LargeStruct value between
// make_large and first_field prevents LLVM from folding the chain
// to `i`. The Sky wrapper still gets inlined but the LargeStruct
// bytes must actually flow through memory across the boundary.

use std::time::Instant;

fn main() {
    let n: usize = 10_000_000;
    let start = Instant::now();
    let mut acc: i64 = 0;
    for i in 0..n {
        let big = unsafe { __lang_stubs::make_large(std::hint::black_box(i as i64)) };
        let big = std::hint::black_box(big);
        acc = acc.wrapping_add(unsafe { __lang_stubs::first_field(big) });
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(acc);
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
