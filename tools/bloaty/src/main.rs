//! bloaty: where does the code in a Rust project live, and where is it bloated?

mod findings;
mod fnmetrics;
mod lines;
mod measure;
mod metrics;
mod names;
mod rollup;
mod scan;
mod shape;
mod smell;
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
    long_about = include_str!("help/main.txt").trim_end()
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
    /// Machine-readable output. Its first field, `schema`, is the format
    /// version (currently 3), bumped when fields change.
    #[arg(long)]
    json: bool,
    /// List every file that couldn't be read or fully analysed.
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Code lines split into implementation, test, doc and comment.
    #[command(
        long_about = include_str!("help/summary.txt").trim_end()
    )]
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
        long_about = include_str!("help/findings.txt").trim_end()
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
    /// Per-fn cognitive and cyclomatic complexity, nesting, parameters, panic sites and clones.
    #[command(
        long_about = include_str!("help/metrics.txt").trim_end()
    )]
    Metrics {
        #[command(flatten)]
        common: Common,
        /// Show the N most complex fns; 0 shows all (text only).
        #[arg(long, default_value_t = 25)]
        top: usize,
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
        Cmd::Summary { common, .. }
        | Cmd::Findings { common, .. }
        | Cmd::Metrics { common, .. } => common,
    };
    let opts = scan::ScanOptions {
        test_features: common.test_features.clone(),
        no_test_features: common.no_test_features.clone(),
        max_depth,
        // Line counts and metrics only need the requested crates' own
        // module trees; smell (`summary --by file`) ranks against the whole
        // linked workspace, whatever PATH is.
        focus_crates_only: matches!(
            cli.cmd,
            Cmd::Summary {
                by: Some(GroupBy::Crate | GroupBy::Module) | None,
                ..
            } | Cmd::Metrics { .. }
        ),
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
        Cmd::Metrics { common, top } => {
            let report = metrics::report(&scan);
            if common.json {
                to_json(&report)
            } else {
                metrics::render(&report, &scan, top)
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

/// Version of the `--json` output, its first field `schema`. Bumped when a
/// field consumers rely on changes or appears (the Neovim integration
/// refuses older output rather than misread it).
const SCHEMA: u32 = 12;

#[derive(serde::Serialize)]
struct Versioned<'a, T> {
    schema: u32,
    #[serde(flatten)]
    body: &'a T,
}

fn to_json(v: &impl serde::Serialize) -> String {
    let v = Versioned {
        schema: SCHEMA,
        body: v,
    };
    serde_json::to_string_pretty(&v).expect("serializable") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// The help text states each threshold as the code has it.
    #[test]
    fn help_states_the_thresholds() {
        use crate::measure::Measure::{CognitiveComplexity as Cog, Lines};
        use crate::{shape, smell};
        let flat = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let summary = flat(include_str!("help/summary.txt"));
        let metrics = flat(include_str!("help/metrics.txt"));
        let findings = flat(include_str!("help/findings.txt"));
        let pct = |share: f64| (share * 100.0).round() as u32;
        for (text, phrase) in [
            (
                &summary,
                format!(">= {} impl lines and a fn", smell::MIN_IMPL_LINES),
            ),
            (
                &summary,
                format!(
                    "a dimension >= {} (at most {}% of files)",
                    smell::HOT_DIM,
                    pct(smell::HOT_SHARE)
                ),
            ),
            (
                &summary,
                format!(
                    "one >= {} (at most {}% more, none under {} files)",
                    smell::WARM_DIM,
                    pct(smell::WARM_SHARE),
                    smell::MIN_FILES_FOR_WARM
                ),
            ),
            (
                &summary,
                format!(
                    "a fn of >= {} lines, cognitive complexity >= {} or the panic density",
                    Lines.bar(),
                    Cog.bar(),
                ),
            ),
            (
                &summary,
                format!(
                    "from {} lines or cognitive complexity {} the allow is ignored",
                    Lines.allow_ceiling(),
                    Cog.allow_ceiling(),
                ),
            ),
            (
                &summary,
                format!(
                    "small arms (<= {} code lines and cognitive complexity <= {}",
                    shape::SMALL_ARM_LINES,
                    shape::SMALL_ARM_COMPLEXITY
                ),
            ),
            (
                &metrics,
                format!("No entry may exceed {} code lines", shape::SMALL_ARM_LINES),
            ),
            (
                &summary,
                format!(
                    ">= {} lines with nothing nested: no cognitive increment over {}",
                    Lines.bad_arm(),
                    crate::measure::FLAT_MAX_INCREMENT
                ),
            ),
            (
                &findings,
                format!(
                    "clippy::too_many_lines (below {} lines)",
                    Lines.allow_ceiling()
                ),
            ),
            (
                &summary,
                format!(
                    "arm of >= {} lines or cognitive complexity >= {} keeps",
                    Lines.bad_arm(),
                    Cog.bad_arm(),
                ),
            ),
            (
                &summary,
                format!(
                    "a fn of >= {} lines or cognitive complexity >= {}, and the panic density",
                    Lines.bad_arm(),
                    Cog.bad_arm(),
                ),
            ),
            (
                &summary,
                format!("at least {} tokens", shape::MIN_PRELUDE_TOKENS),
            ),
            (
                &summary,
                format!("the first {} copies", shape::MAX_PRELUDE_AT),
            ),
            (
                &metrics,
                format!("after at most {} lets", shape::MAX_LEAD_STMTS),
            ),
            (
                &metrics,
                format!(
                    ">= {} simple entries spanning >= {} code lines",
                    shape::MIN_ENTRIES,
                    shape::MIN_ENTRIES
                ),
            ),
            (
                &metrics,
                format!(
                    ">= {} arms, at least half of them small (<= {} code lines and cognitive complexity <= {}",
                    shape::MIN_ARMS,
                    shape::SMALL_ARM_LINES,
                    shape::SMALL_ARM_COMPLEXITY
                ),
            ),
            (
                &findings,
                format!(
                    "At least {} tests at any size, or a pair with --min-dup lines",
                    crate::findings::parametrize::MIN_PARAMETRIZE_TESTS
                ),
            ),
            (
                &findings,
                format!(
                    "at least {} piece of logic",
                    crate::findings::duplicate::MIN_LOGIC
                ),
            ),
        ] {
            assert!(text.contains(&phrase), "help text lacks: {phrase}");
        }
    }

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
