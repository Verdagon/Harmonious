# Writing `fn main()` in toylang

Two rules that are easy to miss when porting a Rust snippet to toylang.
Violating either compiles and links cleanly; the program then crashes
at runtime (rule 1) or during rustc codegen (rule 2).

## Rule 1: `fn main()` must return void

Terminate the last statement of `main` with `;` unless its return
type is already unit.

**Wrong** — `Write::write_all(...)` returns `Result<(), Error>`, which
becomes `main`'s tail expression and therefore its return type:

```
fn main() {
    Write::write_all(&stdout(), b"hi\n")
}
```

Compiles. Prints `hi`. Then SIGBUSes (exit 138 on macOS) before the
process can exit cleanly. See @MBMRVZ for the full cross-cutting
explanation.

**Right** — terminate with `;` so main's tail is implicit unit:

```
fn main() {
    Write::write_all(&stdout(), b"hi\n");
}
```

An explicit void-returning extern function as the tail also works
(this is what the integration tests do):

```
fn println_i32(x: i32)

fn main() {
    Write::write_all(&stdout(), b"hi\n");
    println_i32(0i32)
}
```

Every other toylang function is fine — only `main` has the quirk,
because its extern-ABI wrapper is pinned to `fn main() -> ()` by the
Rust shim that calls it.

## Rule 2: `use`-import every Rust type that touches the type system

Not just the ones you name in the source. Types that flow through
implicitly also need imports. See @RTMEIZ for the full mechanism.

The set of types you need to import includes:

- **Every type you name directly** — `Uuid`, `Vec`, `Regex`, etc.
- **The `Self` type of every trait-method call.** `Write::write_all(&stdout(), ...)`
  has `Self = Stdout`. You never wrote `Stdout` in the source, but it
  still needs `use std::io::Stdout`.
- **The return type of every tail expression**, even when the tail
  has a trailing `;` and the value is discarded. `Result<(), Error>`
  needs `use std::result::Result` and `use std::io::Error`.
- **Every non-primitive generic type argument** inside the above.
  `Result<(), Error>` → both `Result` and `Error`.

### Worked example: a minimal uuid program

```
use uuid::Uuid
use std::io::stdout
use std::io::Stdout
use std::io::Write
use std::result::Result
use std::io::Error

fn main() {
    let id = Uuid::new_v4();
    Write::write_all(&stdout(), b"uuid ok\n");
}
```

Six imports for a 3-line body. Per-line justification:

| Import | Needed because |
|---|---|
| `uuid::Uuid` | Named: `Uuid::new_v4()` |
| `std::io::stdout` | Named: the `stdout()` free function call |
| `std::io::Stdout` | Implicit: `Self` of `Write::write_all(&stdout(), ...)` |
| `std::io::Write` | Named: the trait in `Write::write_all(...)` |
| `std::result::Result` | Implicit: return type of the tail-`;` expression |
| `std::io::Error` | Implicit: generic arg inside `Result<(), Error>` |

If any of these is missing, toylangc panics during either
type-resolve (`after_rust_analysis`) or codegen
(`generate_and_compile`) with `Rust type '<name>' not found` at
`oracle.rs:112`. The trace tells you which code path wanted the type
and therefore which kind of usage (Self / return / generic-arg)
triggered the lookup.

## Pre-flight checklist for a new toylang program

Before running `toylangc build`, walk the body and answer:

1. Is the last statement's return type void? If not, add `;`.
2. For every `Trait::method(&receiver, ...)` call: is `typeof(receiver)`
   `use`-imported?
3. For every expression that returns a Rust generic type
   (`Result<T, E>`, `Option<T>`, etc.): are `T` and `E` — and the
   generic type itself — all `use`-imported?

If the answer to any of (2)-(3) is "I don't know what `typeof(...)` is",
build-and-fail is faster than staring at the source. The panic tells
you the name, and you add the import and rerun.

## See also

- `docs/arcana/MainBodyMustReturnVoid-MBMRVZ.md` — full ABI-mismatch
  explanation for rule 1.
- `docs/arcana/RustTypesMustBeExplicitlyImported-RTMEIZ.md` — full
  type-registry explanation for rule 2.
- `toylangc/tests/integration_tests.rs::test_stdout_write_all`,
  `test_write_all_result_bound` — canonical working patterns for
  I/O in toylang.
- `toylangc/tests/standalone/uuid_test/main.toylang` — canonical
  standalone project.
