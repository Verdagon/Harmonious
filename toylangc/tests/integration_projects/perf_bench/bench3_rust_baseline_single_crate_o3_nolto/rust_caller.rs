// Bench 3 pure-Rust baseline — single-crate variant. Widget + impl Drop
// + make_widget are all defined HERE in the user_bin, so under O3 LLVM's
// intra-crate inliner can already eliminate the Drop body without LTO.
// Use this as the upper-bound on what nolto LTO can deliver in a
// "best case scenario for nolto" sense — sanity check that LTO doesn't
// somehow LOSE optimization opportunities. The interesting comparison
// is bench3_rust_baseline_cross_crate_* (Widget in a sibling crate)
// vs Sky's bench3_drop_*.
use std::time::Instant;

struct Widget {
    id: i32,
}

impl Drop for Widget {
    fn drop(&mut self) {}
}

#[inline(never)]
fn make_widget(id: i32) -> Widget {
    Widget { id }
}

fn main() {
    let n: usize = 10_000_000;
    let mut v: Vec<Widget> = Vec::with_capacity(n);
    for i in 0..n {
        v.push(make_widget(i as i32));
    }
    let start = Instant::now();
    drop(v);
    let elapsed = start.elapsed();
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
