// Bench 3 pure-Rust baseline — cross-crate variant. Widget + impl Drop
// + make_test_widget live in the test_widgets sibling crate (path-dep'd
// in toylang.toml). This is the apples-to-apples structural equivalent
// of Sky's bench3_drop_*: cross-crate Drop impl, no intra-crate
// inlining shortcut. The nolto-vs-thin ratio here is the meaningful
// comparison against Sky's 26.5×.
use std::time::Instant;
use test_widgets::Widget;

fn main() {
    let n: usize = 10_000_000;
    let mut v: Vec<Widget> = Vec::with_capacity(n);
    for i in 0..n {
        v.push(test_widgets::make_test_widget(i as i32));
    }
    let start = Instant::now();
    drop(v);
    let elapsed = start.elapsed();
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
