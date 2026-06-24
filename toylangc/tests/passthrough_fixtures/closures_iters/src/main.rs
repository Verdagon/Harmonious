//! Closures + iterator combinators pass-through fixture. Each
//! closure type gets its own monomorphisation; iterator combinators
//! produce nested generic types. Catches Sky's per_instance_mir
//! provider misbehaving on pure-Rust generic chains.

fn main() {
    let nums: Vec<i64> = (1..=100).collect();

    // Multi-stage iterator chain. Each adapter is a generic type.
    let total: i64 = nums.iter()
        .copied()
        .filter(|n| n % 3 == 0 || n % 5 == 0)
        .map(|n| n * 2)
        .take_while(|n| *n < 500)
        .sum();

    // Capture-by-value closure exercising FnMut.
    let mut counter = 0i32;
    let mut bump = |inc: i32| { counter += inc; counter };
    let _ = bump(1); let _ = bump(2); let final_counter = bump(3);

    // Capture-by-reference closure exercising Fn.
    let table = vec!["a","b","c","d","e"];
    let pick = |i: usize| table[i % table.len()];
    let picked: Vec<&str> = (0..7).map(pick).collect();

    // FnOnce + move semantics.
    let owned = String::from("once");
    let consume = move || owned.len();
    let consumed_len = consume();

    println!("total={}", total);
    println!("counter={}", final_counter);
    println!("picked={:?}", picked);
    println!("consumed_len={}", consumed_len);
}
