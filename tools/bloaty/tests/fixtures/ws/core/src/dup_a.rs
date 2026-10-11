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

/// Short, but with logic: identical free fns are duplicates however short.
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
    pub fn twice(&self) -> u32 { self.0 * 2 }
}

impl Right {
    pub fn twice(&self) -> u32 { self.0 * 2 }
}
