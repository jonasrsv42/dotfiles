// Support code: an example, measured but never scored.
use fxcore::fragile::Registry;

fn main() {
    let mut r = Registry::new();
    for line in ["1 = one", "2 = two", "3 = three"] {
        r.parse(line);
    }
    let first = r.first().to_string();
    let second = r.name(2).to_string();
    assert_eq!(first, "one");
    assert_ne!(second, first);
    let joined = [first, second].join(",");
    println!("{joined}");
    std::process::exit(if r.len() == 3 { 0 } else { 1 });
}
