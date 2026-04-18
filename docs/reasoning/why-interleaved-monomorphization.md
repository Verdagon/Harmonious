# Why `rustc-lang-facade` Uses Interleaved Monomorphization

This document answers a question that's foundational to understanding
the `rustc-lang-facade` architecture: **why does the facade hook into
rustc's monomorphization phase at all?** Couldn't toylang just do a
pre-pass over its own source, enumerate everything, and hand rustc a
list of what to compile?

The short answer: pre-pass works for some cases (specifically, the
ones where every concrete type argument originates in toylang source)
but fails for others (when rustc-compiled code originates concrete
type arguments that toylang can't see). We walk through the full case
taxonomy with complete program examples so you can see exactly where
the line is, and then explain how interleaved monomorphization handles
the cases that pre-pass can't.

If you're trying to figure out whether your consumer architecture
*needs* interleaving, the summary table at the bottom is where to
jump first. The body of this doc is the justification for that
verdict.

**A note on scope.** The current toylang test corpus exercises only
**Case 2** (toylang-as-top-level calls Rust library code with concrete
type arguments chosen in toylang source). The `uuid_test`,
`indexmap_test`, `regex_test`, etc. under `toylangc/tests/standalone/`
are all Case 2 — they build toylang as the top-level program and
link Rust libraries below it. **Cases 1a, 1b, 3, 5 all assume
Rust-as-top-level calling toylang-as-library**, which the current
build flow doesn't wire up. **Cases 4 and 6 assume trait impls on
consumer types**, which toylang's current frontend doesn't
implement. They're included here because they illustrate the
architectural cases the facade was *designed* to support, not
because toylang exercises them today. The mechanisms discussed here
are what the facade *enables*; whether a given consumer frontend
chooses to exercise them is a separate question, answered by that
frontend's feature set. Vale's planned interop story (see §"Why
interleaving is the general-case answer" below) sits in cases the
current toylang doesn't reach.

**Syntactic conventions.** All examples are macro-free. Output uses
`stdout().write_all(b"...")` (trait method, not `println!`). Vector
construction uses `Vec::new()` + explicit `.push()` (not `vec![]`).
The point is to keep the code examples close to what the type system
actually sees — macros expand before type checking, but for reading
purposes they hide what's happening.

---

## Background: the compilation problem

When toylang and Rust compile together, every concrete generic
instantiation in the final binary has to exist exactly once. Toylang
emits some symbols via its own LLVM backend; rustc emits the rest.
They must agree on which `(item, concrete_type_args)` tuples each
side produces — otherwise you get linker errors (duplicate
definitions, or undefined symbols).

There are two broad strategies for coordinating this:

1. **Pre-pass.** Before rustc starts, toylang walks its source,
   computes the reachable set of `(item, concrete_args)` tuples, and
   hands rustc a static list. Rustc compiles what's on the list;
   toylang compiles what it owns.

2. **Interleaved.** Toylang plugs in as a query provider and/or
   codegen hook inside rustc's own pipeline. As rustc's monomorphization
   collector walks through reachable items, it queries toylang for
   bodies, layouts, and symbols of consumer items it encounters.
   Toylang responds per-Instance, with concrete args already
   substituted in.

Both can work, but they cover different problem spaces. What follows
is a seven-case taxonomy of consumer architectures (one top-level
shape has two sub-cases for non-generic vs generic, hence 1a and 1b)
— which ones pre-pass can handle, which ones force interleaving, and
why.

---

## Part 1: What pre-pass can handle

Pre-pass works when **every concrete type argument at every generic
call site in the final reachable set is computable from toylang
source alone.** Two cases satisfy this cleanly.

### Case 1a: Rust program calls a toylang library (non-generic only)

The top-level is a Rust program. It depends on toylang code as a
library. It calls only non-generic toylang entry points.

**Rust top-level (`main.rs`):**

```rust
extern crate toylang_lib;

fn main() {
    toylang_lib::emit_hello();
}
```

**Toylang library:**

```toylang
use std::io::stdout
use std::io::Stdout
use std::io::Write

fn emit_hello() {
    let out = stdout();
    Write::write_all(&out, b"hello\n");
}
```

**What has to exist in the binary:**

- `emit_hello` — toylang-emitted (toylang's LLVM backend).
- `stdout()` — rustc-emitted (stdlib body).
- `<Stdout as Write>::write_all` — rustc-emitted (stdlib body).

**Who originates the concrete type arguments?** Toylang source. The
Rust side doesn't pass any concrete type arguments into toylang
generics (there aren't any generics to parameterize). `Stdout` is
named in toylang's `use` imports.

**Why pre-pass works:** toylang parses its own source, sees the
`stdout()` call and the `Write::write_all::<Stdout>` trait call. It
can emit an anchor function like:

```rust
#[inline(never)]
fn __toylang_rust_anchors() {
    let _ = std::io::stdout as fn() -> std::io::Stdout;
    let _ = <std::io::Stdout as std::io::Write>::write_all
        as fn(&mut std::io::Stdout, &[u8]) -> std::io::Result<()>;
}
```

Inject this anchor into the compilation, and rustc's collector walks
it naturally: it sees the function-pointer casts, queues `stdout` and
`<Stdout as Write>::write_all` for monomorphization, and cascades
from there through all their transitive Rust dependencies. No
per-Instance interleaving needed.

**A word on emission.** For the Rust top-level to *call*
`emit_hello()`, rustc needs to resolve the symbol at link time. In
this pre-pass design, that's handled by the same stub-injection
mechanism the facade uses today: the toylang compiler emits a Rust
shim crate containing `pub fn emit_hello()` declared with
`#[no_mangle]` + an `unreachable!()` body. Rust sees a callable
function; toylang's LLVM backend separately emits the real body at
the same symbol; patch 3 (or its zero-fork equivalent, a codegen
backend plugin) keeps rustc from emitting a conflicting definition.
The point of the Case 1a pre-pass argument isn't that it eliminates
all fork plumbing — it's that the monomorphization-time *discovery*
of reachable Rust deps can be done ahead of time via static anchors
rather than per-Instance queries. The emission side of the story
(who emits what at which symbol) is the same problem in both pre-pass
and interleaved models.

### Case 2: Toylang program calls a Rust library

The top-level is a toylang program. It calls Rust generics with
concrete types chosen by toylang source.

**Toylang top-level (main):**

```toylang
use std::io::stdout
use std::io::Stdout
use std::io::Write
use std::vec::Vec
use std::alloc::Global

fn main() {
    let mut v = Vec::new<i32, Global>();
    v.push(1i32);
    v.push(2i32);
    v.push(3i32);

    let out = stdout();
    Write::write_all(&out, b"done\n");
}
```

**Rust library:** stdlib (implicit).

**What has to exist in the binary:**

- `main` — toylang-emitted.
- `Vec::<i32, Global>::new` — rustc-emitted.
- `<Vec<i32, Global>>::push` — rustc-emitted.
- Drop glue for `Vec<i32, Global>` — rustc-emitted.
- `stdout()`, `<Stdout as Write>::write_all` — rustc-emitted.

**Who originates the concrete type arguments?** Toylang source.
`i32`, `Global`, `Stdout` all appear in toylang's source or `use`
imports. Rustc's collector walks the anchor function toylang emits
and cascades through stdlib's Vec implementation — which itself has
generic dependencies on Allocator and related traits, all of which
rustc handles automatically through its own trait resolution.

**Why pre-pass works:** every concrete instantiation of a Rust generic
comes from a toylang source site. Toylang enumerates them, emits an
anchor, and rustc cascades.

> **Honest aside.** This is the shape the entire current toylang
> test corpus exercises. Every `standalone_tests.rs` smoke test is
> Case 2: toylang-as-top-level calls into a Rust library with
> concrete type args chosen by toylang source. If the framework
> were only ever going to support this shape, a pre-pass design
> would suffice and the 5-patch fork would be overkill. The
> non-trivial cases (1b, 3, 4, 5, 6 below) are what justifies the
> architectural investment — and every one of them requires Rust
> code to either originate a concrete consumer Instance or dispatch
> back through a consumer trait impl. Those are the cases the
> framework was designed for, even though today's frontend doesn't
> exercise them yet.

### What both working cases have in common

In both Case 1a and Case 2, the flow of concrete type arguments is
**unidirectional**. Either toylang source fully determines them (Case
1a and Case 2), or they don't exist at all (Case 1a's non-generic
path). Rustc-compiled code doesn't *introduce* any concrete
instantiations that toylang hasn't already seen.

When that property holds, pre-pass is sufficient. The next section is
about what happens when it doesn't.

---

## Part 2: What pre-pass can't handle

Pre-pass fails when rustc-compiled code originates a concrete
`(consumer_item, concrete_args)` tuple that toylang source doesn't
enumerate. Four flavors of this.

### Case 1b: Rust instantiates toylang generics with Rust-defined types

Still Rust-as-top-level, toylang-as-library, but now the Rust side
invokes a toylang generic with a Rust-defined concrete type.

**Rust top-level (`main.rs`):**

```rust
extern crate toylang_lib;

struct LocalThing {
    value: i32,
}

fn main() {
    let t = LocalThing { value: 42 };
    let wrapped = toylang_lib::wrap::<LocalThing>(t);
    drop(wrapped);
}
```

**Toylang library:**

```toylang
struct Wrapper<T> {
    inner: T,
}

fn wrap<T>(x: T) -> Wrapper<T> {
    Wrapper { inner: x }
}
```

**What has to exist in the binary:**

- `wrap<LocalThing>` — toylang-emitted, with `T = LocalThing`
  substituted in. Needs to know the layout of `LocalThing` to place
  its `inner` field correctly.
- Layout for `Wrapper<LocalThing>` — rustc needs this to allocate
  space when the return value passes through Rust's stack frames.
- Drop glue for `Wrapper<LocalThing>` — needed for the `drop(wrapped)`
  at scope end.

**Who originates the concrete type arguments?** Rust side. `LocalThing`
is defined in `main.rs` and never appears in toylang source. Toylang
has no way, from parsing its own source, to know that someone will
call `wrap<LocalThing>`.

**Side note on how rustc sees `wrap`'s signature.** For rustc to even
type-check `toylang_lib::wrap::<LocalThing>(t)`, it needs `wrap`'s
generic signature. The facade provides this via the generated
`__lang_stubs.rs` module: for each public toylang item, the stub
generator emits a signature-only declaration like
`pub fn wrap<T>(x: T) -> Wrapper<T> { unreachable!() }`. Rustc
type-checks against the stub declaration; when codegen reaches the
call site, the facade's `optimized_mir` override fires to supply the
synthetic dep-registering body (formerly the forked `per_instance_mir`
query, stage-3 migration). This
stub-injection mechanism is what allows the Rust-as-top-level cases
to type-check at all. Pre-pass would still need an equivalent signature
surfacing step — but the harder problem (enumerating concrete
instantiations) happens at codegen time.

**Why pre-pass fails:** toylang's reachable-set enumeration would
have to read every `.rs` file in the Rust top-level crate (and every
dependency), do type checking to resolve which `wrap` instantiations
happen, substitute correctly for generic parameters inside the Rust
code's own call graph, and propagate. That's rustc.

Over-approximation doesn't save you either: `LocalThing` is a user
type defined in the top-level Rust code; toylang has no finite
universe to over-approximate over. Any Rust-defined type could be
passed to `wrap`.

### Case 3: Rust program → toylang library → back into Rust program's code

Rust-as-top-level, toylang-as-library, and the toylang library calls
back into Rust-side code via trait dispatch on a Rust-defined type.

**Contrast with Case 1b.** 1b was a direct instantiation: Rust
top-level called `wrap::<LocalThing>(t)` and the chain stopped
there. Case 3 adds a second hop — the toylang library's body
*itself* dispatches back into Rust-defined code (a `Clone` impl
that lives in `main.rs`). Both fail pre-pass, but for stacked
reasons: 3 inherits 1b's "can't enumerate Rust-defined concrete
args" problem, and adds a "walking toylang source to discover
transitive Rust trait-impl calls" requirement on top.

**Rust top-level (`main.rs`):**

```rust
extern crate toylang_lib;

use std::clone::Clone;

struct MyCounter {
    count: i32,
}

impl Clone for MyCounter {
    fn clone(&self) -> MyCounter {
        MyCounter { count: self.count }
    }
}

fn main() {
    let c = MyCounter { count: 0 };
    let copied = toylang_lib::clone_it::<MyCounter>(&c);
    drop(copied);
}
```

**Toylang library:**

```toylang
use std::clone::Clone

fn clone_it<T>(x: &T) -> T {
    Clone::clone(x)
}
```

**What has to exist in the binary:**

- `clone_it<MyCounter>` — toylang-emitted, with `T = MyCounter`
  substituted.
- `<MyCounter as Clone>::clone` — **rustc-emitted** (the impl body
  lives in `main.rs`, Rust-side code).

**Who originates the concrete type arguments?** Rust side. `MyCounter`
is defined in `main.rs`. The concrete Instance `clone_it<MyCounter>`
is reachable because the Rust top-level calls it.

**Why pre-pass fails:** two reasons at once, and only one of them is
solvable in principle.

First, same as Case 1b: toylang can't enumerate `clone_it<MyCounter>`
without walking Rust source. This is the binding constraint — there's
no way around it without doing rustc's job.

Second: *even after* toylang somehow learns that
`clone_it<MyCounter>` is reachable, toylang would need to walk its
own body with `T = MyCounter` substituted to discover that
`<MyCounter as Clone>::clone` is transitively required. This second
problem is solvable in principle — toylang source is walkable once
T is known. The composite problem is what kills pre-pass: because
the first problem (knowing T without reading Rust source) is not
solvable, the second never gets the chance to matter.

Without interleaving, rustc never sees `clone_it<MyCounter>`'s body,
never discovers the `Clone::clone` call, never queues
`<MyCounter as Clone>::clone` for codegen, and the link fails with
an undefined symbol.

### Case 4: Toylang program → Rust library → back into toylang program's code

Toylang-as-top-level, Rust-as-library, and the Rust library calls back
into toylang-side code via trait dispatch on a toylang-defined type.

**Toylang top-level (main):**

```toylang
use some_rust_lib::duplicate
use std::clone::Clone

struct Widget {
    id: i32,
}

impl Clone for Widget {
    fn clone(&self) -> Widget {
        Widget { id: self.id }
    }
}

fn main() {
    let w = Widget { id: 42 };
    let copy = some_rust_lib::duplicate<Widget>(&w);
    drop(copy);
}
```

**Rust library `some_rust_lib` (`lib.rs`):**

```rust
use std::clone::Clone;

pub fn duplicate<T: Clone>(x: &T) -> T {
    x.clone()
}
```

**What has to exist in the binary:**

- `main` — toylang-emitted.
- `duplicate<Widget>` — **rustc-emitted** (Rust body, instantiated
  with `T = Widget`).
- `<Widget as Clone>::clone` — **toylang-emitted** (Clone impl body
  is in toylang source).

**Who originates the concrete type arguments?** Toylang source
(`Widget`). But the *discovery* that `<Widget as Clone>::clone` is
reachable happens when rustc walks `duplicate<Widget>`'s Rust body
and finds `x.clone()`.

**Why pre-pass fails in the general form:** toylang knows it called
`duplicate<Widget>`. It does *not* know, from its own source, what
`duplicate`'s Rust body does internally — that it calls `x.clone()`
and thereby requires `<Widget as Clone>::clone`. To find out, toylang
would have to walk the Rust library's MIR, which is rustc's work.

**The caveat: single-axis cases admit over-approximation.** For basic
trait methods like `Clone`, toylang *could* pre-enumerate: "I have
`impl Clone for Widget`, so I'll emit an anchor for
`<Widget as Clone>::clone` just in case Rust code calls it." This
over-approximates (compiles dead code if Rust never actually calls
clone), but it works for a finite set of trait impls. When you add
trait generic methods (next section), even this workaround dies.

### Cases 5 and 6: transitive library structure

These are compositions of the simpler cases, one hop deeper.

**Case 5 (Rust top → toylang lib → different Rust lib):**

**Rust top-level (`main.rs`):**

```rust
extern crate toylang_lib;

struct Record {
    id: i32,
}

fn main() {
    let r = Record { id: 99 };
    toylang_lib::store_in_vec::<Record>(r);
}
```

**Toylang library:**

```toylang
use std::vec::Vec
use std::alloc::Global

fn store_in_vec<T>(x: T) {
    let mut v = Vec::new<T, Global>();
    v.push(x);
}
```

**Different Rust library:** stdlib (provides `Vec`).

This is Case 1b layered over Case 2. The Rust top-level originates
the concrete type `Record`, which flows through toylang into stdlib's
Vec. If the top were toylang instead of Rust (with the generic
instantiation happening in toylang source), this would reduce to
Case 2 and pre-pass would work. The interleaving requirement is
inherited from the Case 1b top.

**Case 6 (Toylang top → Rust lib → different toylang lib):**

**Toylang top-level (main):**

```toylang
use some_rust_lib::duplicate
use toylang_util::Pair

fn main() {
    let p = Pair::new<i32, i32>(1i32, 2i32);
    let copy = some_rust_lib::duplicate<Pair<i32, i32>>(&p);
    drop(copy);
}
```

**Rust library `some_rust_lib`:**

```rust
use std::clone::Clone;

pub fn duplicate<T: Clone>(x: &T) -> T {
    x.clone()
}
```

**Different toylang library `toylang_util`:**

```toylang
use std::clone::Clone

struct Pair<A, B> {
    first: A,
    second: B,
}

impl<A, B> Clone for Pair<A, B> {
    fn clone(&self) -> Pair<A, B> {
        Pair {
            first: Clone::clone(&self.first),
            second: Clone::clone(&self.second),
        }
    }
}

fn new<A, B>(a: A, b: B) -> Pair<A, B> {
    Pair { first: a, second: b }
}
```

This is Case 4 with the toylang-defined trait impl living in a
separate library instead of the top-level program. The interleaving
requirement is inherited from the Case 4 structure: rustc walks
`duplicate<Pair<i32, i32>>`'s Rust body to discover the `Clone::clone`
call, which resolves to a toylang-defined impl living in
`toylang_util`.

### The pattern

In every failing case, **rustc-compiled code originates a concrete
Instance of a consumer item that toylang source doesn't and can't
enumerate**. Sometimes the concrete type arguments originate at Rust
call sites (Cases 1b, 3); sometimes concrete type arguments originate
in toylang source but flow through Rust's generic machinery into
concrete trait-method Instances that only rustc's collector can
discover (Cases 4, 6). Either way, pre-pass can't see them.

---

## The core problem, precisely

Here's the shared structure across all four failing cases:

> A concrete Instance `(consumer_item_DefId, concrete_args)` becomes
> reachable in the compiled output. The body (or layout, or drop
> glue) for that Instance lives on the consumer side — toylang must
> supply it. But the *discovery* that this Instance is needed happens
> inside rustc's collector walk, triggered either by a direct Rust
> call site (Cases 1b, 3) or by rustc substituting type arguments
> into a Rust library's body and finding trait-method calls that
> resolve to consumer impls (Cases 4, 6).

The key asymmetry: **toylang has access to toylang source; rustc has
access to all source (both Rust and, through the facade, toylang)**.
Toylang alone can't see the full reachable set because rustc is the
only entity that walks Rust source.

### Why over-approximation doesn't rescue pre-pass

A plausible-sounding workaround for Case 4: "toylang has a finite set
of types and a finite set of trait impls on those types — just
pre-enumerate every method of every impl and emit anchors for all of
them. Over-approximates (compiles dead code), but works."

This survives for single-axis cases. It dies for **generic methods on
generic traits** — the case where the trait itself has a type
parameter *and* the method inside has its own type parameter, each
potentially chosen freely at each Rust call site.

**Example. Rust library with a generic method on a generic trait:**

```rust
// Rust library
use std::io::Write;

pub trait Serialize<Format> {
    fn serialize<Writer: Write>(&self, f: Format, w: &mut Writer);
}

pub fn serialize_to<T, F, W>(x: &T, fmt: F, buf: &mut W)
where
    T: Serialize<F>,
    W: Write,
{
    x.serialize(fmt, buf);
}
```

**Toylang program with a Serialize impl:**

```toylang
use some_rust_lib::serialize_to
use some_rust_lib::Serialize
use std::io::stdout
use std::io::Stdout
use std::io::Write

struct Widget {
    id: i32,
}

struct JsonFormat;

impl Serialize<JsonFormat> for Widget {
    fn serialize<Writer>(&self, f: JsonFormat, w: &mut Writer) where Writer: Write {
        // toylang-side body
    }
}

fn main() {
    let w = Widget { id: 42 };
    let fmt = JsonFormat;
    let mut out = stdout();
    serialize_to<Widget, JsonFormat, Stdout>(&w, fmt, &mut out);
}
```

Now look at the Instance rustc ends up monomorphizing when walking
`serialize_to`'s body:

```
<Widget as Serialize<JsonFormat>>::serialize::<Stdout>
```

Three concrete type arguments:

- `Widget` (the Self type — from toylang source)
- `JsonFormat` (the trait's type parameter — from the impl block)
- `Stdout` (the method's type parameter — from the *Rust call site*
  that passes `&mut out`)

The first two are enumerable from toylang source. The third —
`Writer = Stdout` — is chosen by whoever called `serialize_to`. In
this example it happens to be toylang calling; but if another Rust
caller called `serialize_to` with a different buffer type, the same
toylang-defined method body would need a different concrete
instantiation.

For toylang to anchor this under pre-pass, it would need to enumerate
every concrete `Writer` type any Rust caller might ever pass.
Unbounded. The cross-product of (every toylang type with a Serialize
impl) × (every trait type parameter value) × (every possible method
type parameter value rustc might substitute) is, in general, infinite
or at least not computable from toylang source alone.

**The trait-generic-method case kills the pre-pass over-approximation
workaround for trait-dispatch cases.** Simple trait methods admit it;
generic methods on generic traits don't.

---

## Interleaved monomorphization

The facade's response to this problem: **fire per-Instance providers
during rustc's collector walk.** The idea is that rustc's collector
is the one entity that can see the full reachable set — it walks Rust
source naturally, and it walks consumer source through the facade's
providers. Let the collector drive everything.

### The mechanism, in short

When rustc's collector encounters a concrete Instance of a consumer
item (function, trait-impl method, drop glue, layout), it queries the
facade. The facade responds with a MIR body (for functions and
methods), layout info (for types), or drop-glue MIR (for destructors)
— each specific to the concrete args rustc supplied.

Today this is implemented via:

- `optimized_mir` (query override, DefId-keyed, post stage-3) —
  supplies synthetic MIR bodies for consumer function DefIds. The body
  typically contains `ReifyFnPointer` casts of any Rust items the
  consumer's body calls — with Param placeholders where the consumer
  fn is generic. Rustc's collector substitutes those Params per
  caller during its walk, the same machinery it applies to every
  generic Rust function. (Pre stage-3 this was a custom Instance-keyed
  `per_instance_mir` query with pre-substituted bodies; stage-3
  migrated to Approach B per `docs/reasoning/dep-discovery-approaches.md`.)
- `layout_of` (query, Ty-keyed) — supplies layout data for consumer
  type instantiations.
- `mir_shims` (query, InstanceKind::DropGlue-keyed) — supplies drop
  glue for consumer types.
- `symbol_name` (query, Instance-keyed) — maps consumer Instances to
  consumer-defined symbol names; also the site where the facade
  drives its internal-callee walk (which, unlike Rust-dep discovery,
  requires concrete args — no downstream substitutor exists for
  toylang LLVM IR).

Each of these fires reactively, as rustc's collector encounters
concrete things it needs information about. The consumer side doesn't
have to enumerate anything ahead of time; it just responds to queries.

### The one-way handoff

A useful mental model: **the facade tells rustc the leaves; rustc
walks the rest.**

Consumer code may reference Rust items as part of its body. Consumer
code is invisible to rustc unless the facade surfaces it. So the
facade's job, when rustc queries for a consumer Instance's body, is
to synthesize a body that mentions all the Rust items the consumer
body needs — typically via `ReifyFnPointer` casts of their concrete
DefIds with substituted args. Rustc's collector then walks those
`ReifyFnPointer` statements as part of its normal traversal, sees
the references as uses, queues those Rust items for monomorphization,
cascades into their own bodies, and discovers whatever they reach.

This means **the consumer never has to reimplement rustc's generic
resolution machinery**. It just enumerates the leaves (direct
dependencies) for each Instance. Rustc's collector handles trait
resolution, associated type projection, default methods, blanket
impls, specialization, transitive walks through other generic
functions — all of it. The facade only needs to supply:
"here's this concrete Instance's body, with its direct Rust
dependencies listed."

### How each failing case is handled

**Case 1b: Rust instantiates toylang generics with Rust-defined types.**
Rustc's collector walks `main.rs`, sees `wrap::<LocalThing>(t)`,
queues `wrap<LocalThing>`. Queries `optimized_mir(wrap_def_id)` on
the facade; the facade returns a generic MIR body with a
`ReifyFnPointer` cast per Rust dep (for this minimal example,
essentially none — `wrap` just constructs a struct) and Params in
place of `T`. The collector substitutes `T = LocalThing` as it walks
the body under this particular caller, queuing the concrete Rust
deps for emission. Rustc's codegen for the consumer symbol is skipped
by `CODEGEN_SKIP_HOOK`; toylang's backend emits the real body
separately. Queries `layout_of(Wrapper<LocalThing>)` for stack-frame
allocation. Queries `mir_shims` for the drop glue. All handled
per-Instance via rustc's substitution machinery.

**Case 3: Rust → toylang → back into Rust.** Rustc walks main, queues
`clone_it<MyCounter>`. Facade's `optimized_mir` override fires on
`clone_it_def_id`, returns a generic synthetic body mentioning
`<Param(T) as Clone>::clone` as a `ReifyFnPointer` cast. Rustc's
collector walks it with `T = MyCounter`, substitutes, queues
`<MyCounter as Clone>::clone` — a Rust-defined item (the impl block
is in `main.rs`). Rustc compiles it normally. The facade never has to
know about `MyCounter` or its Clone impl beyond "the `clone_it<T>`
body calls `Clone::clone`, so pass that through as a dep."

**Case 4: Toylang → Rust → back into toylang.** Toylang emits `main`
with a call to `duplicate<Widget>` (an extern declaration referencing
the Rust-emitted symbol). Rustc's collector queues `duplicate<Widget>`
(a normal Rust generic monomorphization), walks its Rust body,
substitutes `T = Widget`, sees `x.clone()`, resolves to
`<Widget as Clone>::clone`. Queries `optimized_mir` on the
consumer-impl DefId — the facade recognizes it as a consumer trait
impl, returns the toylang-defined `clone` body with Params intact;
the collector substitutes `T = Widget` per caller. Rustc's codegen
for the symbol is skipped by `CODEGEN_SKIP_HOOK`; toylang's backend
emits the real body separately.

**Case 5 and 6: transitive library structures.** Compose the above.
Nothing structurally new — the interleaving is already the mechanism
each individual hop needs.

**Case 1a and Case 2 (the pre-pass-compatible cases).** Interleaving
also handles these trivially; it just does more work than needed.
Rustc's collector walks the Rust or toylang top-level; when it
encounters any consumer item, the facade responds; when it encounters
any Rust item, rustc handles it normally. The facade's queries fire,
but because the reachable set is fully enumerable from toylang
source, the answers the facade provides could equivalently have come
from a pre-pass.

### Why interleaving is the general-case answer

A consumer architecture that wants to support *all* of the cases
above — including Rust callers instantiating consumer generics, and
toylang values flowing through Rust generics that dispatch back
through consumer trait impls — must use interleaving. Pre-pass is
correct only for the strict subset where every concrete type
argument in the final reachable set originates in consumer source
(Cases 1a and 2).

The facade chose interleaving for architectural generality: once
you've built interleaving, you get Cases 1a and 2 for free (just a
richer version of the machinery than those cases strictly need),
and you also get every more-complex case. Pre-pass would have made
the framework only usable for a limited consumer shape, and any
user's attempt to add trait impls on consumer types or expose
generics to Rust callers would have required rearchitecting.

Vale (another systems language currently evaluating this architecture
for its Rust interop story) has a planned interop model — Vale types
participating in Rust trait systems, Vale closures flowing into Rust
generic APIs, etc. — that sits firmly in the interleaving-only
region. Toylang today exercises mostly Case 2, but the mechanisms
support the richer cases already.

### Interleaving is the invariant; the specific mechanism is implementation

This is the meta-observation that makes the rest of the design
analysis possible. The architectural principle is "some rustc-side
hook fires per-Instance during monomorphization to supply consumer
bodies." The specific hook is an implementation detail that can be
swapped.

**Today's shipping implementation (post stage 3)** uses a
`rustc_interface::Config::override_queries` override on rustc's
existing `optimized_mir` query — the sanctioned extension point
that rust-analyzer, clippy, and miri all use. Paired with two
small consumer-agnostic fork hooks (`CODEGEN_SKIP_HOOK` in
`rustc_codegen_ssa` and `VISIBILITY_OVERRIDE_HOOK` in
`rustc_monomorphize`, both `OnceLock<fn ptr>` statics the facade
fills at startup). Fork state: 2 patches.

*Historical note: before the stage-3 migration (2026-04-18), this
role was played by a custom `per_instance_mir` query plus four
supporting fork patches. The forked query was never load-bearing;
what's load-bearing is the interleaving behavior. The stage-3
migration swapped the query implementation while preserving the
interleaving contract. See `docs/reasoning/dep-discovery-approaches.md`
for the Approach A (retired) vs Approach B (shipping) comparison.*

Further fork reduction is possible but not currently shipping:

- The remaining `CODEGEN_SKIP_HOOK` patch could be retired by
  pairing with a `CodegenBackend` plugin (see
  `docs/reasoning/rustc-fork-design-space.md` §4.2).
- `VISIBILITY_OVERRIDE_HOOK` could be retired via a separate-crate
  stub model (§4.3) that keeps `#![feature(linkage)]` out of
  user-visible crates.

`docs/reasoning/rustc-fork-design-space.md` §4.1–4.3 + Part 5
analyzes these paths and their current cost estimates. The design-
space doc takes "interleaving is required" as given (from this
doc) and asks the follow-up question: *which specific rustc
mechanism* should implement it. Separating the "why" (this doc)
from the "how" (that doc) keeps each question answerable in
isolation: if the remaining fork patches ever need to be retired
— for Vale's distribution story or otherwise — the migration path
doesn't have to relitigate whether interleaving is needed. It is;
that's settled here.

---

## Summary table

| Case | Top | Middle | Bottom | Concrete args originate in | Pre-pass works? | Reason |
|------|-----|--------|--------|---------------------------|-----------------|--------|
| 1a | Rust | — | toylang (non-generic only) | N/A | Yes | No generics involved |
| 1b | Rust | — | toylang | Rust (via `wrap::<LocalThing>`) | No | Rust originates consumer Instance |
| 2 | toylang | — | Rust | toylang | Yes | All args enumerable from toylang source |
| 3 | Rust | toylang | Rust (same top) | Rust (type), flows through | No | Rust-defined type triggers Rust trait impl via toylang's body |
| 4 | toylang | Rust | toylang (same top) | toylang (type), flows through | No (in general) | Toylang type flows through Rust generic, triggers toylang trait impl discovered via rustc's walk of Rust body |
| 5 | Rust | toylang | different Rust | Rust (via top-level) | No | Inherits from Case 1b |
| 6 | toylang | Rust | different toylang | toylang | No | Inherits from Case 4 |

## Practical guidance

**Building a consumer?** Figure out which cases your consumer
architecture allows. If it's strictly Cases 1a + 2 (no Rust-side
origination of consumer Instances, no consumer types with trait
impls that Rust code might invoke), a pre-pass design is sufficient
and lighter-weight. Otherwise, you need interleaving.

**Evaluating the rustc-lang-facade for your project?** If your
planned interop needs Cases 1b, 3, 4, or their transitive cousins,
the facade's interleaving mechanism is why it exists. If your
planned interop is strictly Cases 1a and 2, the facade is
over-engineered for your needs and a simpler pre-pass design would
suffice.

**Debugging a "missing symbol" link error?** Check whether the
missing symbol is a Case 1b / Case 3 / Case 4 / Case 5 / Case 6
Instance — something whose discovery needed rustc's collector to
walk a body the facade should have supplied. If yes, the bug is
likely a missing per-Instance hook registration or a synthetic MIR
body that omitted a `ReifyFnPointer` it should have included.

---

## See also

- `docs/architecture/rust-interop-guide.md` Part 8 — companion
  section that summarizes the interleaving argument and defers to
  this doc for the full taxonomy, while providing the specific code
  citations and query-provider details for the current facade
  implementation.
- `docs/reasoning/rustc-fork-design-space.md` — takes the
  "interleaving is required" conclusion as given and asks the
  follow-up question: *which rustc-side mechanism* should implement
  interleaving? Parts 2–4 analyze `override_queries` on
  `optimized_mir`, `CodegenBackend` plugins, the separate-crate
  stub model, and `rustc_public` for fork-reduction. If you're
  deciding whether to reduce the fork (as opposed to understanding
  why it exists), start there.
- `docs/arcana/` — cross-cutting concerns in the current
  implementation, each annotated at the code sites that embody it.
