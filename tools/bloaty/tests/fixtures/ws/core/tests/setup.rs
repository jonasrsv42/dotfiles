// Whole-test file: shared test setup (fixture candidates).

struct Db {
    rows: Vec<u32>,
}

fn open(_name: &str) -> Db {
    Db { rows: Vec::new() }
}

fn add(db: &mut Db, v: u32) -> usize {
    db.rows.push(v);
    db.rows.len() - 1
}

fn total(db: &Db) -> u32 {
    db.rows.iter().sum()
}

// Three tests open with the same four statements: a candidate.
#[test]
fn totals_three_rows() {
    let mut db = open("ledger");
    add(&mut db, 1);
    add(&mut db, 2);
    let last = add(&mut db, 3);
    assert_eq!(total(&db), 6);
    assert_eq!(last, 2);
}

#[test]
fn removes_the_last_row() {
    let mut db = open("ledger");
    add(&mut db, 1);
    add(&mut db, 2);
    let last = add(&mut db, 3);
    db.rows.remove(last);
    assert_eq!(total(&db), 3);
}

#[test]
fn clears_every_row() {
    let mut db = open("ledger");
    add(&mut db, 1);
    add(&mut db, 2);
    let last = add(&mut db, 3);
    db.rows.clear();
    assert!(db.rows.get(last).is_none());
}

// Only two tests open alike: no candidate.
#[test]
fn a_pair_sums() {
    let mut db = open("pair");
    add(&mut db, 4);
    add(&mut db, 5);
    add(&mut db, 6);
    assert_eq!(total(&db), 15);
}

#[test]
fn a_pair_counts() {
    let mut db = open("pair");
    add(&mut db, 4);
    add(&mut db, 5);
    add(&mut db, 6);
    assert_eq!(db.rows.len(), 3);
}

// Three tests share three statements only: no candidate.
#[test]
fn short_sums() {
    let mut db = open("short");
    add(&mut db, 7);
    add(&mut db, 8);
    assert_eq!(total(&db), 15);
}

#[test]
fn short_counts() {
    let mut db = open("short");
    add(&mut db, 7);
    add(&mut db, 8);
    assert_eq!(db.rows.len(), 2);
}

#[test]
fn short_pops() {
    let mut db = open("short");
    add(&mut db, 7);
    add(&mut db, 8);
    assert_eq!(db.rows.pop(), Some(8));
}

// Three tests share five statements; a fourth shares only the first two
// and must not hide them: a candidate of the three.
#[test]
fn queue_drains_in_order() {
    let mut db = open("queue");
    add(&mut db, 1);
    add(&mut db, 10);
    add(&mut db, 100);
    let n = total(&db);
    assert_eq!(db.rows.drain(..).collect::<Vec<_>>(), [1, 10, 100]);
    assert_eq!(n, 111);
}

#[test]
fn queue_totals() {
    let mut db = open("queue");
    add(&mut db, 1);
    add(&mut db, 10);
    add(&mut db, 100);
    let n = total(&db);
    assert_eq!(db.rows.len() as u32, 3 + n - 111);
}

#[test]
fn queue_keeps_the_last() {
    let mut db = open("queue");
    add(&mut db, 1);
    add(&mut db, 10);
    add(&mut db, 100);
    let n = total(&db);
    db.rows.truncate(2);
    assert_eq!(db.rows.last(), Some(&(n - 101)));
}

#[test]
fn queue_stray() {
    let mut db = open("queue");
    add(&mut db, 1);
    assert_eq!(total(&db), 1);
}

// Six tests share four statements, three of them nine: each test is in
// one candidate. The three replace 3 x 9 statements, more than 6 x 4, so
// they come first; the other three still share four.
#[test]
fn log_boots() {
    let mut db = open("log");
    add(&mut db, 2);
    add(&mut db, 3);
    add(&mut db, 5);
    assert_eq!(db.rows.first(), Some(&2));
}

#[test]
fn log_loads() {
    let mut db = open("log");
    add(&mut db, 2);
    add(&mut db, 3);
    add(&mut db, 5);
    assert!(db.rows.contains(&3));
}

#[test]
fn log_runs() {
    let mut db = open("log");
    add(&mut db, 2);
    add(&mut db, 3);
    add(&mut db, 5);
    db.rows.reverse();
    assert_eq!(db.rows[0], 5);
}

#[test]
fn log_replays() {
    let mut db = open("log");
    add(&mut db, 2);
    add(&mut db, 3);
    add(&mut db, 5);
    add(&mut db, 7);
    add(&mut db, 11);
    add(&mut db, 13);
    add(&mut db, 17);
    add(&mut db, 19);
    assert_eq!(total(&db), 77);
}

#[test]
fn log_rotates() {
    let mut db = open("log");
    add(&mut db, 2);
    add(&mut db, 3);
    add(&mut db, 5);
    add(&mut db, 7);
    add(&mut db, 11);
    add(&mut db, 13);
    add(&mut db, 17);
    add(&mut db, 19);
    db.rows.rotate_left(1);
    assert_eq!(db.rows[7], 2);
}

#[test]
fn log_trims() {
    let mut db = open("log");
    add(&mut db, 2);
    add(&mut db, 3);
    add(&mut db, 5);
    add(&mut db, 7);
    add(&mut db, 11);
    add(&mut db, 13);
    add(&mut db, 17);
    add(&mut db, 19);
    db.rows.retain(|&r| r > 10);
    assert_eq!(db.rows.len(), 4);
}

// The same opening in cfg-exclusive mods of one name: different scopes (at
// most one compiles), so two tests and one: no candidate.
#[cfg(unix)]
mod imp {
    use super::*;

    #[test]
    fn unix_sums() {
        let mut db = open("imp");
        add(&mut db, 4);
        add(&mut db, 6);
        add(&mut db, 8);
        assert_eq!(total(&db), 18);
    }

    #[test]
    fn unix_counts() {
        let mut db = open("imp");
        add(&mut db, 4);
        add(&mut db, 6);
        add(&mut db, 8);
        assert_eq!(db.rows.len(), 3);
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    #[test]
    fn windows_pops() {
        let mut db = open("imp");
        add(&mut db, 4);
        add(&mut db, 6);
        add(&mut db, 8);
        assert_eq!(db.rows.pop(), Some(8));
    }
}
