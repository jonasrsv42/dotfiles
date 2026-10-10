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

The crates around PATH are analysed together with every crate linked to them
(same workspace, or path/patch dependencies either way), so test code and
cross-crate uses are seen; output is narrowed to PATH. Unrelated crates in
the same repository are left out.

Test code is anything under tests/ or benches/, behind a cfg that implies test
(cfg(test), all(test, ..), any(test, feature = \"testdb\"), any(test, doc),
#![cfg(test)], and loom/fuzzing builds), a #[test]-style fn, or a module
declared from test code. Cargo features named like test/testing/test-utils/
testdb/mock/fake count as test-only; adjust with --test-feature /
--no-test-feature.

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
        /// Group rows by crate, module or file [default: crate when PATH
        /// spans several crates, else module]. File paths are relative to
        /// the current directory when it's inside the repository, else to
        /// the repository root.
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
  unused             fns/types/traits nothing references, or only tests
                     (incl. doctests) do. Name-based, so verify: after
                     removing, run `cargo check --workspace --all-targets
                     --all-features` (or the project's usual feature sets)
                     and `cargo test --doc`, then re-run bloaty (removals can
                     expose more). Code only built under cfg(loom)/fuzzing
                     counts as a use. `uncertain: true` marks crates with
                     code bloaty can't see.
  single-impl-trait  traits with at most one impl (derives and impls in other
                     workspace crates count; blanket/macro impls and extension
                     traits on foreign types are exempt). Fine as a mocking or
                     plugin boundary; otherwise inline.
  long-fn            more than --long-fn own code lines (nested items excluded).
  test-heavy         files whose inline tests are >3x their code (and >= 200):
                     look for repeated setup to share and cases to merge
                     into table-driven tests.
  pass-through       (only with --kind pass-through) private-ish fns whose
                     body is one call passing their inputs to another
                     workspace fn. Mostly a judgement call.

Within a kind, impl code ranks before test code and private before pub.
`category` says impl or test. `lines` counts code lines: the item's own
(plus its impl blocks for unused types), the largest copy for duplicates,
test lines for test-heavy. `lines_saved` is at most what keeping one copy
saves.

JSON per finding: `path` (absolute) and `file` (relative to `root`), `line`
(the name, to jump to), `start_line`..=`end_line` (the whole item with docs
and attributes, to delete), `crate`, and for some kinds `impls`/`copies`
with the same fields (`copies[0]` is the finding itself). `totals` per kind
say what --top cut.

`pub` items of publishable libraries (anything not `publish = false`) are
left out of unused, single-impl-trait and pass-through, since code outside
the repository may use them: `hidden_pub` counts them. Mark internal crates
`publish = false` (or `publish.workspace = true` with the workspace's
`publish = false`) to have them checked, or pass --include-pub.

The JSON report also has `root`, `thresholds`, `test_features`, `totals` and
`shown` per kind (what --top kept)."
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
        /// Also report `pub` items of publishable libraries (not `publish =
        /// false`), which may be API for code outside the repository.
        #[arg(long)]
        include_pub: bool,
    },
}

fn main() {
    // syn recurses once per nesting level (~5.5 KiB each), and rayon may run
    // work on the calling thread, so everything runs inside a pool with big
    // stacks, and files nested deeper than the stack allows (with a 3x
    // margin, capped at 2000 levels) are counted but not parsed.
    let threads = threads_within_address_limit();
    let mut stack = 64 << 20;
    let pool = loop {
        let built = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(stack)
            .build();
        match built {
            Ok(pool) => break pool,
            Err(e) if stack <= 16 << 20 => {
                eprintln!("bloaty: can't start threads: {e}");
                std::process::exit(2);
            }
            Err(_) => stack /= 2,
        }
    };
    let max_depth = (stack / (16 << 10)).min(2000);
    let code = pool.install(|| run(max_depth));
    // Skip freeing every syntax fact one by one: the OS does it at once.
    std::process::exit(code);
}

/// Under an address-space limit (`ulimit -v`), each thread's stack and
/// malloc arena reserve ~256 MiB: use only as many threads as fit.
fn threads_within_address_limit() -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let limit = std::fs::read_to_string("/proc/self/limits")
        .ok()
        .and_then(|l| {
            let line = l.lines().find(|l| l.starts_with("Max address space"))?;
            line.split_whitespace().nth(3)?.parse::<usize>().ok()
        });
    match limit {
        Some(bytes) => (bytes / (256 << 20)).clamp(1, cores),
        None => cores,
    }
}

fn run(max_depth: usize) -> i32 {
    let cli = Cli::parse();
    let common = match &cli.cmd {
        Cmd::Summary { common, .. } | Cmd::Findings { common, .. } => common,
    };
    let opts = scan::ScanOptions {
        test_features: common.test_features.clone(),
        no_test_features: common.no_test_features.clone(),
        max_depth,
        // Line counts only need the requested crates' own module trees.
        focus_crates_only: matches!(cli.cmd, Cmd::Summary { .. }),
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
                findings::render(&report, &scan)
            }
        }
    };
    let mut stdout = std::io::stdout().lock();
    let written = stdout
        .write_all(out.as_bytes())
        .and_then(|_| stdout.flush());
    std::mem::forget(scan);
    match written {
        // A closed pipe (`bloaty ... | head`) is not an error.
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            eprintln!("bloaty: writing output: {e}");
            1
        }
        _ => 0,
    }
}

fn to_json(v: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(v).expect("serializable") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent_and_accepts_json_kind_names() {
        Cli::command().debug_assert();
        for kind in [
            "pass-through",
            "pass_through",
            "single_impl_trait",
            "long-fn",
        ] {
            let cli = Cli::try_parse_from(["bloaty", "findings", "--kind", kind]);
            assert!(cli.is_ok(), "{kind}");
        }
    }
}
