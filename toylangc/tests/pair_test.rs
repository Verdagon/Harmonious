mod __lang_stubs;
use __lang_stubs::*;

fn make_pair() -> Pair<i32, i32> { unreachable!() }

fn main() {
    let p = make_pair();
    println!("Pair: {} {}", p.first, p.second);
    assert_eq!(p.first, 10);
    assert_eq!(p.second, 20);
}
