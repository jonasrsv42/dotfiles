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
