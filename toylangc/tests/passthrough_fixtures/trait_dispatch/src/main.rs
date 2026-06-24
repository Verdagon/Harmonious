//! Trait dispatch pass-through fixture. Exercises rustc's static +
//! dynamic dispatch codegen with Sky machinery dormant.

trait Animal {
    fn name(&self) -> &'static str;
    fn legs(&self) -> u32;
}

struct Dog;
impl Animal for Dog {
    fn name(&self) -> &'static str { "dog" }
    fn legs(&self) -> u32 { 4 }
}

struct Spider;
impl Animal for Spider {
    fn name(&self) -> &'static str { "spider" }
    fn legs(&self) -> u32 { 8 }
}

fn describe_static<A: Animal>(a: &A) -> String {
    format!("{}/{}", a.name(), a.legs())
}

fn describe_dyn(a: &dyn Animal) -> String {
    format!("{}/{}", a.name(), a.legs())
}

fn main() {
    let d = Dog;
    let s = Spider;
    let zoo: Vec<&dyn Animal> = vec![&d, &s];
    let descriptions: Vec<String> = zoo.iter().map(|a| describe_dyn(*a)).collect();
    println!("static-dog={}", describe_static(&d));
    println!("static-spider={}", describe_static(&s));
    println!("dyn={:?}", descriptions);
}
