//! bloaty: where does the code in a Rust project live, and where is it bloated?

mod findings;
mod lines;
mod scan;
mod summary;
#[cfg(test)]
mod testutil;
mod visit;

use clap::{Args, Parser, Subcommand};
use findings::{Category, Kind};
use std::io::Write;
use std::path::PathBuf;
use summary::{GroupBy, SortBy};

#[derive(Parser)]
#[command(
    version,
    about = "Where does the code in a Rust project live, and where is it bloated?",
    long_about = "Where does the code in a Rust project live, and where is it bloated?

Workflow: `bloaty summary` to see where the code is, drill in with
`--by module --depth N` or a PATH, then `bloaty findings PATH --json` (or
`--kind duplicate` etc.) for ranked places to cut. Findings are hints: check
each one, and compile after removing anything.

The whole repository/workspace around PATH is always analysed (so test code
and cross-crate uses are seen); output is narrowed to PATH.

Test code is anything under tests/ or benches/, behind a cfg that implies test
(cfg(test), all(test, ..), any(test, feature = \"testdb\"), #![cfg(test)]), a
#[test]-style fn, or a module declared from test code. Cargo features named
like test/testing/test-utils/testdb/mock/fake count as test-only; adjust with
--test-feature / --no-test-feature.

Files ignored by .gitignore and cargo target dirs are skipped."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct Common {
    /// Files or directories to report on.
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    /// Treat this cargo feature like cfg(test) (repeatable).
    #[arg(long = "test-feature", value_name = "FEATURE")]
    test_features: Vec<String>,
    /// Don't treat this auto-detected feature like cfg(test) (repeatable).
    #[arg(long = "no-test-feature", value_name = "FEATURE")]
    no_test_features: Vec<String>,
    /// Machine-readable output.
    #[arg(long)]
    json: bool,
    /// List every file that couldn't be read or fully analysed.
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Code lines split into implementation, test, doc and comment.
    Summary {
        #[command(flatten)]
        common: Common,
        /// Group rows by crate, module or file [default: crate for a
        /// workspace, module for a single crate].
        #[arg(long, value_enum)]
        by: Option<GroupBy>,
        /// Module path components kept when grouping by module, after
        /// dropping `src/` (node/biunion is 2).
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Order rows by implementation, test or all code lines.
        #[arg(long, value_enum, default_value = "impl")]
        sort: SortBy,
        /// Show the N largest groups; 0 shows all.
        #[arg(long, default_value_t = 0)]
        top: usize,
    },
    /// Ranked hints of bloat: where to look, not what to delete.
    #[command(
        long_about = "Ranked hints of bloat: where to look, not what to delete.

Kinds, in the order worth working through:
  duplicate          fns with identical bodies (or, for long ones, the same
                     shape with different names): extract one helper.
                     `identical` says which; copies across crates need a
                     shared dependency.
  unused             fns/types/traits nothing references, or only tests do.
                     Name-based: confirm by compiling after removal. `pub`
                     items of library crates are hidden (--include-pub).
  single-impl-trait  traits with at most one impl (derives and impls in other
                     workspace crates count; blanket/macro impls and extension
                     traits on foreign types are exempt). Fine as a mocking or
                     plugin boundary; otherwise inline.
  pass-through       fns whose body is one call passing their own inputs
                     along. Private ones with few refs are easiest to inline.
  long-fn            more than --long-fn own code lines (nested items excluded).
  test-heavy         files whose inline tests are >3x their code (and >= 200).

Within a kind, impl code ranks before test code and private before pub.
`category` says impl or test. `lines` is the item's own code lines; for
duplicates, `lines_saved` is at most what keeping one copy saves.
`uncertain: true` marks items in crates with code bloaty can't see.
JSON gives absolute `path` plus `file` relative to `root`, `line`/`end_line`,
and `totals` per kind so you know what --top cut."
    )]
    Findings {
        #[command(flatten)]
        common: Common,
        /// Only these kinds (repeatable).
        #[arg(long, value_enum)]
        kind: Vec<Kind>,
        /// Only impl or only test code.
        #[arg(long, value_enum)]
        category: Option<Category>,
        /// Show at most N findings per kind; 0 shows all.
        #[arg(long, default_value_t = 25)]
        top: usize,
        /// Own code lines above which a fn counts as long.
        #[arg(long, default_value_t = 50)]
        long_fn: usize,
        /// Minimum own code lines for a fn to be checked for duplicates.
        #[arg(long, default_value_t = 8)]
        min_dup: usize,
        /// Also report `pub` items of library crates as unused or
        /// single-impl (usually API for code outside the workspace).
        #[arg(long)]
        include_pub: bool,
    },
}

fn main() {
    // syn recurses once per nesting level (~5 KiB each), and rayon may run
    // work on the calling thread, so everything runs inside a pool with big
    // stacks. 64 MiB covers ~10k levels; scan skips files deeper than that.
    // Address-space limits (ulimit -v) can refuse the reservation, so ask
    // for less until it fits.
    let mut stack = 64 << 20;
    let pool = loop {
        match rayon::ThreadPoolBuilder::new().stack_size(stack).build() {
            Ok(pool) => break pool,
            Err(e) if stack <= 8 << 20 => {
                eprintln!("bloaty: can't start threads: {e}");
                std::process::exit(2);
            }
            Err(_) => stack /= 2,
        }
    };
    let code = pool.install(run);
    // Skip freeing every syntax fact one by one: the OS does it at once.
    std::process::exit(code);
}

fn run() -> i32 {
    let cli = Cli::parse();
    let common = match &cli.cmd {
        Cmd::Summary { common, .. } | Cmd::Findings { common, .. } => common,
    };
    let opts = scan::ScanOptions {
        test_features: common.test_features.clone(),
        no_test_features: common.no_test_features.clone(),
    };
    let scan = match scan::scan(&common.paths, &opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bloaty: {e}");
            return 2;
        }
    };
    if common.verbose {
        for (file, why) in &scan.skipped {
            eprintln!("bloaty: skipped {file}: {why}");
        }
    } else if !scan.skipped.is_empty() {
        eprintln!(
            "bloaty: {} files unreadable or not fully analysed (their lines still count); -v lists them",
            scan.skipped.len()
        );
    }
    if !scan.files.iter().any(|f| f.in_focus) {
        let paths: Vec<_> = common
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        eprintln!("bloaty: no .rs files under {}", paths.join(", "));
        return 1;
    }

    let out = match cli.cmd {
        Cmd::Summary {
            common,
            by,
            depth,
            sort,
            top,
        } => {
            let s = summary::summary(&scan, by, depth, sort, top);
            if common.json {
                to_json(&s)
            } else {
                summary::render(&s)
            }
        }
        Cmd::Findings {
            common,
            kind,
            category,
            top,
            long_fn,
            min_dup,
            include_pub,
        } => {
            let opts = findings::Options {
                long_fn,
                min_dup,
                include_pub,
            };
            let all = findings::findings(&scan, &opts);
            let report = findings::report(&scan, &all, &opts, &kind, category, top);
            if common.json {
                to_json(&report)
            } else {
                findings::render(&report)
            }
        }
    };
    // A closed pipe (`bloaty ... | head`) is not an error.
    let mut stdout = std::io::stdout().lock();
    let _ = stdout
        .write_all(out.as_bytes())
        .and_then(|_| stdout.flush());
    std::mem::forget(scan);
    0
}

fn to_json(v: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(v).expect("serializable") + "\n"
}
