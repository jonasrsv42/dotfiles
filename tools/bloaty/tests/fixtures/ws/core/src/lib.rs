//! fxcore: the fixture's library.

// Reached from test code first (breadth-first, in declaration order), then
// from impl code: impl wins, so shared.rs is not test code.
#[cfg(test)]
#[path = "shared.rs"]
mod shared_for_tests;
// A `#[path]` file behaves like mod.rs: its `mod data;` is testing/data.rs.
#[cfg(test)]
#[path = "testing/support.rs"]
mod support;

pub mod allowed;
pub mod badarm;
pub mod branchy;
pub mod dispatch;
pub mod dup_a;
pub mod fragile;
pub mod hot;
pub mod long;
pub mod routes;
pub mod shapes;
pub mod shared;
pub mod f01;
pub mod f02;
pub mod f03;
pub mod f04;
pub mod f05;
pub mod f06;
pub mod f07;
pub mod f08;
pub mod f09;
pub mod f10;
pub mod f11;
pub mod f12;
pub mod f13;
pub mod f14;
pub mod f15;
pub mod f16;
pub mod f17;
pub mod f18;
pub mod f19;
pub mod f20;

/// Calls every filler, so none is unused.
pub fn run_all(xs: &[u32]) -> u32 {
    let mut total = 0;
    total += f01::f1(xs);
    total += f02::f2(xs);
    total += f03::f3(xs);
    total += f04::f4(xs);
    total += f05::f5(xs);
    total += f06::f6(xs);
    total += f07::f7(xs);
    total += f08::f8(xs);
    total += f09::f9(xs);
    total += f10::f10(xs);
    total += f11::f11(xs);
    total += f12::f12(xs);
    total += f13::f13(xs);
    total += f14::f14(xs);
    total += f15::f15(xs);
    total += f16::f16(xs);
    total += f17::f17(xs);
    total += f18::f18(xs);
    total += f19::f19(xs);
    total += f20::f20(xs);
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_all_is_stable() {
        assert_eq!(run_all(&[]), run_all(&[]));
        let mut r = fragile::Registry::new();
        r.parse("1 = one");
        assert_eq!(r.name(1), "one");
        assert_eq!(support::sample().len(), 4);
        assert_eq!(shared_for_tests::label(1), shared::label(1));
    }
}
