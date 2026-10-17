//! One copy of a duplicated fn (the other is in fxapp's dup_b.rs).

pub fn checksum(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    let mixed = (b << 16) | a;
    let folded = mixed ^ (mixed >> 7);
    folded.rotate_left(3)
}

/// Short: identical free fns below --min-dup need 7 lines, so none.
pub fn plus(a: u32, b: u32) -> u32 { a + b }

pub fn add(a: u32, b: u32) -> u32 { a + b }

#[cfg(test)]
mod tests {
    /// A test copy of `checksum`: in the same group as it and adler.
    fn reference(data: &[u8]) -> u32 {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        let mixed = (b << 16) | a;
        let folded = mixed ^ (mixed >> 7);
        folded.rotate_left(3)
    }

    #[test]
    fn matches_reference() {
        assert_eq!(super::checksum(b"fx"), reference(b"fx"));
        assert_eq!(Framed(vec![1]).window(0, 1, 1), Padded(vec![1]).window(0, 1, 1));
    }

    struct Framed(Vec<u8>);
    struct Padded(Vec<u8>);

    // Long signatures around bodies that only forward to `self`: no duplicate.
    impl Framed {
        fn window(
            &self,
            from: usize,
            to: usize,
            step: usize,
        ) -> Vec<u8> {
            self.0.window(from, to, step)
        }
    }

    impl Padded {
        fn window(
            &self,
            from: usize,
            to: usize,
            step: usize,
        ) -> Vec<u8> {
            self.0.window(from, to, step)
        }
    }
}

/// One body on two types: Self types differ, so a note, not a finding.
pub struct Left(pub u32);
pub struct Right(pub u32);

impl Left {
    pub fn twice(&self) -> u32 {
        let x = self.0 * 2;
        let y = x.wrapping_add(1);
        let z = y.rotate_left(3);
        let w = z ^ x;
        let v = w.wrapping_sub(y);
        v.max(x)
    }
}

impl Right {
    pub fn twice(&self) -> u32 {
        let x = self.0 * 2;
        let y = x.wrapping_add(1);
        let z = y.rotate_left(3);
        let w = z ^ x;
        let v = w.wrapping_sub(y);
        v.max(x)
    }
}

/// Identical free fns below --min-dup, 7 lines after the signature: a
/// duplicate (unlike `plus` and `add`).
pub fn mix(a: u32, b: u32) -> u32 {
    let x = a ^ b;
    let y = x.wrapping_mul(31);
    let z = y.rotate_left(5);
    let w = z ^ (z >> 3);
    let v = w.wrapping_add(a);
    v ^ b
}

pub fn blend(a: u32, b: u32) -> u32 {
    let x = a ^ b;
    let y = x.wrapping_mul(31);
    let z = y.rotate_left(5);
    let w = z ^ (z >> 3);
    let v = w.wrapping_add(a);
    v ^ b
}

/// Same shape as its test's reference copy below (names differ): a
/// note, since only one copy is impl code.
pub fn spread(xs: &[u32]) -> u32 {
    let mut lo = u32::MAX;
    let mut hi = 0;
    for &x in xs {
        if x < lo {
            lo = x;
        }
        if x > hi {
            hi = x;
        }
    }
    if lo > hi {
        return 0;
    }
    let width = hi - lo;
    width + 1
}

#[cfg(test)]
mod spread_tests {
    fn spread_ref(ys: &[u32]) -> u32 {
        let mut low = u32::MAX;
        let mut high = 0;
        for &y in ys {
            if y < low {
                low = y;
            }
            if y > high {
                high = y;
            }
        }
        if low > high {
            return 0;
        }
        let span = high - low;
        span + 1
    }

    #[test]
    fn matches_reference() {
        assert_eq!(super::spread(&[3, 9]), spread_ref(&[3, 9]));
    }
}
