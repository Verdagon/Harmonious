use std::time::Instant;

fn main() {
    let n: usize = 10_000_000;
    let mut v: Vec<__lang_stubs::Widget> = Vec::with_capacity(n);
    for i in 0..n {
        v.push(__lang_stubs::make_widget(i as i32));
    }
    let start = Instant::now();
    drop(v);
    let elapsed = start.elapsed();
    println!("BENCH_ELAPSED_US={}", elapsed.as_micros());
}
