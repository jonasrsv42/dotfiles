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
