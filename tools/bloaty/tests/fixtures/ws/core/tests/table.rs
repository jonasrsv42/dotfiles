// Whole-test file: table-driven-test candidates (parametrize).

fn clamp(v: u32, lo: u32, hi: u32) -> u32 {
    assert!(lo <= hi);
    v.max(lo).min(hi)
}

// Four tests differing only in literals (one an identical copy): a
// candidate at any size.
#[test]
fn clamps_below() {
    assert_eq!(clamp(1, 3, 9), 3);
}

#[test]
fn clamps_above() {
    assert_eq!(clamp(12, 3, 9), 9);
}

#[test]
fn keeps_inside() {
    assert_eq!(clamp(5, 3, 9), 5);
}

#[test]
fn keeps_inside_again() {
    assert_eq!(clamp(5, 3, 9), 5);
}

// The same body expecting a panic: other attributes, not in the group.
#[test]
#[should_panic]
fn rejects_inverted_bounds() {
    assert_eq!(clamp(5, 9, 3), 5);
}

// A short pair: below the threshold.
#[test]
fn parses_one() {
    assert_eq!("1".parse::<u8>(), Ok(1));
}

#[test]
fn parses_two() {
    assert_eq!("2".parse::<u8>(), Ok(2));
}

// A long pair whose attributes differ (one ignored): no group.
#[test]
fn sorts_small() {
    let mut v: Vec<u32> = (0..10).rev().collect();
    v.sort();
    assert_eq!(v.len(), 10);
    assert_eq!(v[0], 0);
    assert_eq!(v[9], 9);
    assert!(v.windows(2).all(|w| w[0] <= w[1]));
    v.dedup();
    assert_eq!(v.len(), 10);
}

#[test]
#[ignore]
fn sorts_large() {
    let mut v: Vec<u32> = (0..100000).rev().collect();
    v.sort();
    assert_eq!(v.len(), 100000);
    assert_eq!(v[0], 0);
    assert_eq!(v[99999], 99999);
    assert!(v.windows(2).all(|w| w[0] <= w[1]));
    v.dedup();
    assert_eq!(v.len(), 100000);
}

mod windows {
    fn sums(data: &[u32], size: usize) -> Vec<u32> {
        data.windows(size).map(|w| w.iter().sum()).collect()
    }

    // A long pair in an inline mod: a candidate.
    #[test]
    fn sums_pairs() {
        let data = [1, 2, 3, 4, 5];
        let s = sums(&data, 2);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0], 3);
        assert_eq!(s[1], 5);
        assert_eq!(s[2], 7);
        assert!(s.iter().all(|x| *x > 0));
    }

    #[test]
    fn sums_triples() {
        let data = [1, 2, 3, 4, 5];
        let s = sums(&data, 3);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], 6);
        assert_eq!(s[1], 9);
        assert_eq!(s[2], 12);
        assert!(s.iter().all(|x| *x > 0));
    }
}

// Inline snapshots (insta): `@"…"` is the expected output, not data: no
// group.
#[test]
fn snapshot_below() {
    insta::assert_snapshot!(clamp(1, 3, 9), @"3");
}

#[test]
fn snapshot_above() {
    insta::assert_snapshot!(clamp(12, 3, 9), @"9");
}

#[test]
fn snapshot_inside() {
    insta::assert_snapshot!(clamp(5, 3, 9), @"5");
}

// One test in cfg-exclusive mods: at most one compiles, no group.
#[cfg(unix)]
mod imp {
    #[test]
    fn separator() {
        assert_eq!(std::path::MAIN_SEPARATOR, '/');
    }
}

#[cfg(windows)]
mod imp {
    #[test]
    fn separator() {
        assert_eq!(std::path::MAIN_SEPARATOR, '\\');
    }
}

#[cfg(target_os = "wasi")]
mod imp {
    #[test]
    fn separator() {
        assert_eq!(std::path::MAIN_SEPARATOR, '?');
    }
}

// A char and a byte can't share a table column: no group.
#[test]
fn code_of_char() {
    assert_eq!(u32::from('B'), 66);
}

#[test]
fn code_of_byte() {
    assert_eq!(u32::from(b'B'), 66);
}

#[test]
fn code_of_next_char() {
    assert_eq!(u32::from('C'), 67);
}

// Tuple fields are structure, not data: no group.
fn bounds() -> (u32, u32, u32) {
    (3, 5, 9)
}

#[test]
fn low_bound() {
    assert!(bounds().0 < 10);
}

#[test]
fn mid_bound() {
    assert!(bounds().1 < 10);
}

#[test]
fn high_bound() {
    assert!(bounds().2 < 10);
}

// A pair long only by its strings' text (3 lines with code each): below
// the threshold.
#[test]
fn reads_small_table() {
    let text = "name = small
size = 1
kind = a
owner = x
group = y
order = 1
level = 2";
    assert!(text.lines().count() > 3);
}

#[test]
fn reads_large_table() {
    let text = "name = large
size = 100
kind = b
owner = z
group = w
order = 9
level = 8";
    assert!(text.lines().count() > 3);
}

// Literals inside a macro other than the well-known expression macros are
// its input, not data: no group.
#[test]
fn reads_first_sample() {
    assert!(!include_str!("first.txt").is_empty());
}

#[test]
fn reads_second_sample() {
    assert!(!include_str!("second.txt").is_empty());
}

#[test]
fn reads_third_sample() {
    assert!(!include_str!("third.txt").is_empty());
}
