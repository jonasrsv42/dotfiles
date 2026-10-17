//! fxapp: the fixture's binary.

mod dup_b;

use fxcore::branchy::classify;
use fxcore::fragile::Registry;
use fxcore::hot::{plan, total, Job};
use fxcore::long::{describe, Config};
use fxcore::shapes::{largest, Square};

fn main() {
    let xs: Vec<u32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let jobs: Vec<Job> = xs
        .iter()
        .map(|&id| Job { id, deps: vec![], cost: id % 7, urgent: id % 3 == 0 })
        .collect();
    let mut reg = Registry::new();
    reg.add(0, "zero");
    let config = Config {
        name: reg.first().to_string(),
        port: 8080,
        host: "localhost".into(),
        retries: 2,
        verbose: reg.len() > 1,
    };
    println!("{}", describe(&config));
    println!("{:?} of {}", plan(&jobs, 20), total(&jobs));
    println!("{} {}", fxcore::run_all(&xs), fxcore::shared::label(1));
    println!("{}", fxcore::dup_a::checksum(b"fx") ^ dup_b::adler(b"fx") ^ fxcore::dup_a::plus(1, fxcore::dup_a::add(2, 3)) ^ fxcore::dup_a::mix(1, fxcore::dup_a::blend(2, 3)) ^ fxcore::dup_a::spread(&xs) ^ fxcore::dup_a::Left(1).twice() ^ fxcore::dup_a::Right(2).twice());
    println!("{}", largest(&[Square(2.0)]) + f64::from(classify('x', false)));
    let stats = fxcore::allowed::Stats { done: 1, failed: 0, queued: 2, workers: 1 };
    println!("{:?}", fxcore::allowed::report(&stats));
    let routes = fxcore::routes::routes();
    println!("{}", routes.iter().filter(|r| r.auth).map(|r| (r.handler)(r.path.len() as u32)).sum::<u32>());
    println!("{}", routes.iter().map(|r| r.method).collect::<String>());
    let mut log = Vec::new();
    let cmd = fxcore::dispatch::Cmd::Add(fxcore::dispatch::Req { id: "1".into(), name: "a".into() });
    println!("{}", fxcore::dispatch::weight(&cmd));
    println!("{:?}", fxcore::dispatch::run(cmd, &mut log));
    let mut stack = xs.clone();
    println!("{}", fxcore::badarm::apply(fxcore::badarm::Op::Rebalance { limit: 3, strict: true }, &mut stack));
}

#[cfg(test)]
mod tests {
    /// The same as fxcore's tests/it.rs `sample_ids`: short test code in
    /// two crates, a note.
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
    fn samples_every_third_id() {
        assert_eq!(sample_ids(7), vec![0, 3, 6]);
    }
}
