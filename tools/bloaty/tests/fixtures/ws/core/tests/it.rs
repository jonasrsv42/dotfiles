// Whole-test file: counted as tests, never scored.
use fxcore::hot::{plan, Job};

#[test]
fn plans_urgent_jobs_first() {
    let jobs = vec![
        Job { id: 1, deps: vec![], cost: 5, urgent: false },
        Job { id: 2, deps: vec![], cost: 3, urgent: true },
    ];
    let order = plan(&jobs, 10);
    assert_eq!(order.first(), Some(&1));
    assert!(order.contains(&2));
}

/// Same shape as `odds` (names and literals differ), both test code: a note.
fn evens(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for &x in xs {
        if x % 2 == 0 {
            out.push(x);
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.len() > 3 {
        out.truncate(3);
    }
    out.reverse();
    out.retain(|&v| v > 0);
    out
}

fn odds(ys: &[u32]) -> Vec<u32> {
    let mut kept = Vec::new();
    for &y in ys {
        if y % 2 == 1 {
            kept.push(y);
        }
    }
    kept.sort_unstable();
    kept.dedup();
    if kept.len() > 4 {
        kept.truncate(4);
    }
    kept.reverse();
    kept.retain(|&v| v > 1);
    kept
}

/// The same as fxapp's test `sample_ids`: short test code in two crates.
fn sample_ids(n: u32) -> Vec<u32> {
    let mut ids = Vec::new();
    for id in 0..n {
        if id % 3 == 0 {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[test]
fn splits_sample_ids() {
    let ids = sample_ids(12);
    assert!(evens(&ids).len() <= 3);
    assert!(odds(&ids).iter().all(|v| v % 2 == 1));
}
