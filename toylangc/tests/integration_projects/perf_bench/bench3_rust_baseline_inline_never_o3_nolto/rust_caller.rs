// Bench 3 pure-Rust baseline — inline_never variant. WidgetNoInline
// lives in the test_widgets sibling crate with `#[inline(never)]` on
// its Drop impl, so the per-element drop is a real function call even
// under fat LTO. The nolto-vs-thin ratio here is the "irreducible
// per-drop cost" floor: what does the chain cost when the inliner
// literally can't help? Compare to Bench 1's nolto baseline
// (~0.87ns/call) to sanity-check the cross-crate call cost story.
use std::time::Instant;
use test_widgets::WidgetNoInline;

fn main() {
    let n: usize = 10_000_000;
    let mut v: Vec<WidgetNoInline> = Vec::with_capacity(n);
    for i in 0..n {
        v.push(test_widgets::make_test_widget_no_inline(i as i32));
    }
    let start = Instant::now();
    drop(v);
    let elapsed = start.elapsed();
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
