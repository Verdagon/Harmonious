mod __lang_stubs;
use __lang_stubs::*;

fn make_counter() -> Counter { unreachable!() }
fn wrap_value(x: i32) -> Counter { unreachable!() }

fn main() {
    let c = make_counter();
    println!("Counter value: {}", c.value);
    assert_eq!(c.value, 42);

    let c2 = wrap_value(99);
    println!("Wrapped value: {}", c2.value);
    assert_eq!(c2.value, 99);
}
