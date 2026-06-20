// Case 1b architectural shape: Rust top calls Sky generic with a
// **Rust-user-defined** T. The earlier i32 version exercised the mechanism
// (non-empty instance.args) but used a stdlib type; sharpened here to
// LocalThing — a struct defined in main.rs that Sky has never seen at
// frontend time. Sky's per_instance_mir fires with
// instance.args = [LocalThing] and must produce a body that knows
// LocalThing's layout without ever parsing main.rs.

struct LocalThing {
    value: i32,
}

fn main() {
    let t = LocalThing { value: 42 };
    let r = __lang_stubs::identity::<LocalThing>(t);
    test_helpers::println_i32(r.value);
}
