# bloaty: architecture

bloaty answers two questions about a Rust workspace: where does the code
live (line counts split into impl / test / doc / comment), and where does
it smell (long, branchy or panicky fns, ranked against the rest of the
workspace; plus bloat findings such as duplicates and unused items). The
CLI is in this directory; the Neovim side is lua/mbloaty/ at the repository
root. User-facing behaviour is documented in `bloaty <cmd> --help`
(src/help/*.txt); this file is for people and agents changing the code.

Principle: only measure what syn shows reliably. No type information, no
guesses at design quality, no weights. bloaty is purely syntactic and
snappy: it may miss things, but never claims what needs semantic analysis
(macro expansion, name resolution, types). If a feature needs types or
name resolution, it's rust-analyzer's job (precision on demand), not
bloaty's. See Limits.

## Pipeline

```
scan ─ visit ─ fnmetrics / shape ─ rollup ─ smell ─┬ summary  ┐
                                                   ├ findings ├ --json ─ Lua cache ─ tree / lens / text
                                                   └ metrics  ┘
```

Rust (src/):

- `main.rs`: clap CLI (`summary`, `findings`, `metrics`), `SCHEMA`, the
  `{"schema": N, ...}` JSON wrapper, help-text tests.
- `help/*.txt`: long help per command (`include_str!`).
- `scan.rs`: finds the workspace around PATH (git root, else outermost
  Cargo.toml), its crates and .rs files (.gitignore, then .bloatyignore),
  parses files in parallel (`analyse`), resolves module trees (`ModWalk`)
  to tell test files and orphans. Reports narrow to PATH; analysis always sees the
  whole workspace.
- `lines.rs`: line classifier (syn drops comments): code, doc, comment,
  blank; `LineCounts`; the nesting depth that keeps absurd files from
  syn. A fold (`Fold`: line marks, depth) over the tokens of
  `lines/tokens.rs`: the lexical grammar (comments, strings, char literals
  vs lifetimes) as parsicomb parsers, dispatched on a token's first byte;
  the two hand-written parts (the streamed `token*` loop, the nested
  comment depth count) say why.
- `visit.rs`: one `syn::Visit` per file → `FileFacts` (test ranges, mod
  decls, imports, `FnFact`s, items, impls, name mentions).
  - `visit/cfg.rs`: which attributes make code test-only / checked-test.
  - `visit/body.rs`: forwarding detection (to other fns as a structured
    `Forward`, to `self`), a
    body's logic count, body hashes (exact/shape/names).
  - `visit/sig.rs`: a fn's `Signature` as text, name left out (generics,
    receiver, parameter types, return type, qualifiers, the impl's Self
    type and trait); `differs` names the parts copies differ in.
  - `visit/macros.rs`: `EXPRESSION_MACROS`, the one allow-list of
    macros whose arguments are read as expressions; every other macro is
    opaque (its words are only mentions, `macro_words`).
  - `visit/syntax.rs`: paths, types and words as text.
- `fnmetrics.rs`: second, small walk per non-test fn body: cyclomatic
  and cognitive complexity (with its worst increment), nesting, params,
  panic sites, clones (`FnMetrics`; one `MetricsWalk::new` for fns, arms,
  remainders and expressions).
- `shape.rs`: per-fn `Shape` (table, dispatch with arms and repeated
  prelude) and allowed clippy lints; `settle` (run in `scan::analyse`)
  drops shapes that don't hold up once code lines are known.
- `measure.rs`: `Measure` {Lines, CognitiveComplexity} (the scored
  per-fn numbers; `Measure::of` reads one off a fn or arm) and `Dim`;
  every per-measure constant (bar, bad-arm/culprit bar, allow ceiling,
  lint, JSON metric name, text) lives here.
- `rollup.rs`: per-file roll-ups: raw (`file_metrics`) and as scored
  (`effective_metrics`, excuses applied); `FnScore`, `Excuse`, blamed arms.
- `smell.rs`: percentiles, dimensions, Pareto fronts, levels, reasons
  (`dims`) and `why`.
- `summary.rs`: line-count groups by crate/module/file; `--by file`
  attaches roll-ups and smell.
- `findings/mod.rs`: `Finding`, `Kind`, `Cx` (reference counts, uncertain
  crates), orchestration, `report`, `render`; one file per kind:
  `duplicate.rs`, `unused.rs`, `single_impl_trait.rs`, `pass_through.rs`,
  `long_fn.rs`, `test_heavy.rs`. Duplicates also have
  `duplicate/policy.rs` (`Rule`: a flat table from a group's facts to its
  tier and hint).
- `metrics.rs`: the `metrics` command: every measured fn, its shape rows
  and panic sites.
- `names.rs`: `Loc`, `short_name`, `collapse_generics`.
- `testutil.rs`: on-disk fixture workspaces for unit tests.

Neovim (lua/mbloaty/):

- `init.lua`: runs `summary --by file --json` and `findings --json --top 0`
  per root, caches them, refuses output older than its `SCHEMA`, `lookup`,
  `metrics` (runs `bloaty metrics FILE`), setup and keys.
- `tree.lua`: the neo-tree column (line counts, `•` findings, `●` smell
  dot: hot red, warm yellow, noted blue) and `B` explain.
- `lens.lua`: end-of-line notes on the culprits of a file's dot, quickfix
  list, fn float.
- `text.lua`: all wording shared by tree and lens (pure functions of JSON).

## Key concepts

- **Impl vs test lines.** Test code: `#[cfg(test)]` (and test features,
  `loom`, `fuzzing`), `#[test]`-like attributes, tests/ and benches/,
  modules declared from test code (whole-test files). Test code is never
  measured or scored.
- **Checked tests.** Test code `cargo check --all-targets --all-features`
  builds. Mentions from unchecked test code (`cfg(loom)`, other platforms)
  count as ordinary uses, so `unused` doesn't flag what they use.
- **Support files.** Build scripts and examples/: measured, never scored.
- **Workspace-wide scoring.** Each signal is a mid-rank percentile among
  all scored files of the workspace (≥ 10 impl lines, a measured fn, not
  test or support). size = max(largest fn, impl lines); complexity =
  cognitive complexity; fragility = panic density (≥ 2 panics, ≥ 30 impl
  lines, ≥ 1/100; fixed bands when under 5 files qualify). Rounded to 2
  decimals before anything compares them.
- **Pareto fronts and caps.** Non-dominated sorting on the three
  dimensions. hot: walking (front, top-decile dims, sum, path), files with
  a dim ≥ 0.9, at most 5%. warm: the rest with a dim ≥ 0.75 by sum, at most
  10% more, none under 20 files. Caps are tight because directories show
  their worst child.
- **Absolute bars.** hot/warm also need a fn ≥ 40 lines, cognitive
  complexity ≥ 15, or qualifying fragility: percentiles alone would flag
  tidy files.
- **Culprit.** A real (not excused) fn dim at or past the bad-arm bar
  (60 lines, cognitive complexity 10), and the panic density, carry
  `culprit: true`; the lens marks only culprits. **Every dot has a
  culprit:** a file is eligible for hot (warm) only past the bar *and*
  with a culprit among the reasons that level would name; an ineligible
  file stays in the population (percentiles unchanged) and its slot goes
  to the next eligible file. Impl lines alone never level a file. **Long
  flat fns cap at warm:** a culprit-sized largest fn (or blamed arm) with
  nothing nested (largest cognitive increment ≤
  `measure::FLAT_MAX_INCREMENT`, 1: an exact syntactic claim) carries
  `flat: true` (lens: `long? N lines · flat`, or `long? arm A N lines ·
  flat (fn M)`, not `split?`); hot needs a culprit that isn't flat (one
  rule, checked at hot eligibility and again after a bad arm keeps the
  raw level). A flat largest fn doesn't hide a culprit-sized nested one:
  the roll-up carries the largest nested fn (`largest_nested`) and the
  reasons name it too. Dims that aren't culprits get no verdict: the
  quickfix and float list them as `also: …`, the lens not at all.
- **Levels.** `hot` / `warm` from the effective ranking; `noted` (blue):
  would be hot/warm by raw numbers (same eligibility), but only because
  of an excused fn whose own number would have been a culprit; only such
  excused signals are listed.
- **Cognitive complexity** is the complexity dimension; cyclomatic
  complexity and nesting are reported breakdown columns (fn, dispatch
  remainder, `max_cyclomatic_complexity` / `max_nesting` raw per file).
  `worst_increment` {line, value} is a fn's single largest increment, ties
  first in source order; the float shows it.
- **dims.** Structured reasons of a levelled file: `{dim, metric, value,
  name?, line?, arm?, panics?, culprit?, flat?, excused?}`. `why` is rendered from
  them, for display only.
- **Shapes.** *Table*: a body ending in one literal of ≥ 4 simple entries;
  its literal's lines are excused (lets, scrutinee and complexity still
  count). *Dispatch*: a tail match of ≥ 4 arms, at least half of them
  small (⌈n/2⌉, by `Arm::small`, one meaning); small arms (≤ 12 code lines in the body and cognitive complexity
  ≤ 3, measured from inside the arm) are excused, the fn counts as
  remainder (lets + scrutinee) plus every non-small arm: lines added,
  cognitive complexity remainder + 1 (the match) + the counted arms'.
  A dispatch arm at or past the bad-arm bar keeps the file at the worse of
  raw and effective level, blaming the arm (`arm`, `shape.worst`).
- **Allow opt-outs.** `#[allow]`/`#[expect]` (also in `cfg_attr`) of
  `clippy::too_many_lines` excuses lines, `clippy::cognitive_complexity`
  cognitive complexity; only on the fn, only below the ceiling (2 × bar:
  80 lines, cognitive complexity 30).
- **Findings kinds.** duplicate, unused, single_impl_trait, long_fn,
  test_heavy, pass_through (opt-in). long_fn counts lines as the smell
  score does (`rollup::lines_excuse`): what a table, dispatch or allowed
  lint excuses under `--long-fn` isn't reported but counted in `excused`
  {kind: {excuse: n}}. Name-based, hints not proof; `pub`
  items of publishable libraries hidden unless `--include-pub`. Claims
  that would need name resolution or macro expansion aren't made:
  `unclaimed` {kind: {reason: n}} counts them (`Cx::exclude`).
- **Duplicates: exact claims, facts, two tiers.** A duplicate finding
  claims the same code more than once: an identical body (or, for >= 15
  lines, the same shape unless only literals differ) and the same written
  `Signature` (the method's and its impl's generics, receiver, types,
  Self type and trait; associated fns that never mention `Self`/`self`
  leave the Self type out). Each copy needs substance (`--min-dup` code
  lines after the signature and >= `MIN_LOGIC` (1) piece of logic,
  `body::logic`: 0 for forwarding and constructors; identical free fns /
  inherent methods of impl code at any size). Each body group is
  reported once with all its copies (several signatures: each signature
  class of >= 2 is a group, and the whole set once more as a note naming
  the classes, so no copy goes silent). Per copy: crate, test, cfgs as
  written, signature, class; per group: crates, `depends` (direct
  Cargo.toml dependencies between them), identical. `policy::Rule::of`
  picks the first row of one table whose single fact holds: signatures
  differ → note; same shape under different cfgs (one crate) → note;
  identical under different cfgs (one crate) / several crates /
  identical / same shape → finding. Hints are advice text. Notes are
  counted apart (`duplicate_notes`). `excluded_duplicates` counts groups
  substance removed (only groups that were duplicates by text size).

## Limits

What each claim means syntactically. Rule: if a feature needs types or
name resolution, it's rust-analyzer's job (precision on demand), not
bloaty's; when a claim would need them, bloaty makes none (counted in
`unclaimed`, never silently dropped).

- **Names aren't resolved.** Same-named items count as the same: a
  mention of `parse` is a mention of every `parse`. Where a claim would
  need to know which one is meant, it isn't made (`unclaimed`
  `same_name`): an unused type whose only mentions sit inside some
  same-named type's definition or impls; a trait whose name another
  workspace trait shares (single-impl); a pass-through to a fn name
  several workspace fns share. Imports are read as text (`use a::B as
  C`), not resolved.
- **Macros are opaque**, except `visit::macros::EXPRESSION_MACROS`
  (std's `format!`, `vec!`, `assert*!`, `debug_assert*!`, `print*!`,
  `write*!`, `panic!`/`unreachable!`/`todo!`/`unimplemented!`,
  `matches!`, `dbg!`, `pin!`; log/tracing's `trace!`..`error!`, `event!`), matched
  by last path segment, whose arguments are read as expressions when they
  parse (otherwise that invocation is opaque too). Any other macro's tokens: every identifier counts as a mention
  (so `unused` never flags what a macro names), a trait named in them makes
  no single-impl claim (`named_in_macro`), and in fn bodies the panic
  calls and clones written there are counted (text the user wrote) but no
  complexity or nesting. An item-position macro whose whole body parses as
  Rust items (tokio's `cfg_rt! { pub mod runtime; }`) is read as those
  items, as written; what it defines is exempt from unused. Nothing else
  is guessed: `mod x;` inside `cfg_if!` or `macro_rules!` templates and
  trait impls in `macro_rules!` bodies aren't seen. Such files are
  orphans: still counted, their impl/test split by path only, and their
  crate is `uncertain`; an orphan whose stem a macro of its crate names
  (`macro_declared`) gets no claim resting on that split (unused,
  long-fn, duplicate groups with a copy there: `unclaimed`
  `named_in_macro`) and no smell level. Cfgs written through macros
  (tokio's `cfg_rt!`/`cfg_not_rt!`) aren't recorded, so copies under them
  read as identical duplicates without cfgs.
- **unused** = the name is mentioned nowhere outside test code (or, for a
  fn, only in its own body). Mentions: identifiers anywhere in code,
  attribute arguments (incl. words in their strings: `#[serde(default =
  "f")]`), macro tokens, doctests (as test mentions), files pulled in by
  `include!`, and every word of a build script's string literals. Not
  mentions: comments, plain doc text, other string literals (in a file
  syn can't parse every word counts). Mentions in definitions and plain
  `use` imports don't count.
- **cognitive complexity** = SonarSource's rules over syntax, with these
  differences: no recursion increment (calls aren't resolved:
  `self.0.fmt(f)` forwards, it doesn't recurse); as in Sonar, only
  control flow and closures nest (`{ }`, `unsafe`, `async` and `const`
  blocks don't, though they are levels of the reported `nesting`); match
  arms nest, guards sit at the match's level; a labelled `break`
  (also out of a labelled block) adds 1 like Sonar's jump to a label;
  boolean runs are counted over a whole `&&`/`||` tree read through
  parentheses (`a && (b && c)` is one run; `!`, calls and other operators
  start a new expression); match guards add nothing, their `&&`/`||` do;
  `let .. else` counts like an `if` (a branch written out, with its own
  block) while `?` adds nothing (error plumbing, the normal path). Macros as
  for cyclomatic complexity: only `EXPRESSION_MACROS` arguments count.
  Arms are measured like fns (from nesting 0).
- **panics** = explicit calls by name: `.unwrap()`, `.expect(x)` (not
  `.expect(x)?`), `panic!`, `unreachable!`, `todo!`, `unimplemented!`,
  `assert!`, `assert_eq!`, `assert_ne!`; a custom `unwrap` method counts,
  indexing, overflow, division and panics inside callees don't.
- **cfg / test classification** approximates the build: cfgs are read as
  text, not evaluated (`cfg(unix)` vs `cfg(target_family = "unix")`
  differ), test-only features by name, files by the `mod` tree bloaty
  can follow.
- **Signatures and types** compare as written text: lifetimes ignored
  (outside `for<..>`), aliases differ from their types, `Self` from the
  concrete type.
- Duplicates: names aren't resolved: same-named items in different files
  or crates (consts, helpers, imported types) count as the same, so equal
  text can be different code. Signatures and cfgs compare as normalized
  text: lifetimes ignored (outside `for<..>`), generic parameter names as
  written (`<T>` vs `<U>`, `<T: X>` vs `where T: X`, predicate order),
  `Self` vs the concrete type, `Vec<u8>` vs an alias all differ;
  `#[cfg(unix)]` and `#[cfg(target_family = "unix")]` differ. Bodies
  differing only in literals are data, never duplicates. bloaty doesn't
  check whether copies can call each other (visibility, re-exports,
  types).

## Invariants

These must always hold; tests/invariants.rs checks most of them on the
fixture and on any `BLOATY_CORPUS`.

- **Scoring is PATH-independent.** A file's smell is the same whether
  bloaty is asked about the workspace, a subdirectory or the file.
- **Panics are never excused.** Fragility is computed the same raw and
  effective. Tables excuse only lines.
- **noted ⊆ raw hot/warm.** A file is noted only if the raw ranking would
  level it and an excused fn is behind that; a noted file has only excused
  dims (each at or past the culprit bar), a hot/warm one at least one real
  dim.
- **Every dot has a culprit.** A hot or warm file has at least one dim
  with `culprit: true`; a hot one, one without `flat`.
- **Schema.** Any change to a JSON field consumers read (rename, new
  field relied on, meaning change) bumps `SCHEMA` in src/main.rs *and*
  `SCHEMA` in lua/mbloaty/init.lua together (a test compares them).
- **Lua reads structure, never `why`.** The lens and tree use `dims`,
  `culprit`, `excused`, `arm`, `most_panics`, `shape`; `why` is shown,
  never parsed.
- **Thresholds live only in Rust** (measure.rs, smell.rs, shape.rs consts).
  The help text states them; `help_states_the_thresholds` in main.rs fails
  if a constant drifts from its phrase. Lua has no thresholds.
- **One source per number.** `panics` is `panic_sites.len()`; the
  group's raw peaks equal its fns' first maxima (ties: first in file);
  `most_panics` is the first fn with the most panics, with its sites.
- **Goldens are behaviour.** tests/golden/*.json change only on purpose,
  with the diff explained.

## Tests

```
./check.sh               # fmt --check, clippy -D warnings, cargo test (incl. dogfood)
./check.sh --install     # ... then cargo install --offline --locked --path .
cargo test --test golden # goldens (UPDATE_GOLDEN=1 rewrites; review the diff)
BLOATY_CORPUS=~/dev/proj:$HOME/.cargo/registry/src/<index>/tokio-1.x \
  cargo test --test invariants -- --nocapture
nvim --headless -u NONE -l tools/bloaty/nvim_test.lua   # from the repo root
```

- Unit tests sit next to their code (`facts`/`Fixture` helpers in
  testutil.rs; findings' shared helpers in `findings::tests`).
- tests/golden.rs: the three JSON outputs on tests/fixtures/ws (copied to a
  temp dir with its own `.git`; see tests/common/mod.rs), plus assertions
  that each fixture role still plays out (hot.rs hot, routes.rs noted, ...).
  The fixture's file count matters: warm needs ≥ 20 scored files and the
  caps must leave room for every role.
- tests/invariants.rs: cross-output consistency (above).
- tests/nvim.rs: runs nvim_test.lua against the test build when nvim is on
  PATH.
- tests/dogfood.rs: bloaty's own fns (its `metrics` JSON) stay under
  cyclomatic complexity 15 unless they allow or expect
  `clippy::cognitive_complexity` under a `//` comment saying why (none
  does today).
- tests/fixtures/.bloatyignore keeps the fixture out of scans of this repo.

## Recipes

**Add a per-fn measure.** Compute it in `fnmetrics.rs` (`MetricsWalk`,
field on `FnMetrics`, unit tests there). Reported only (like clones): add
it to `metrics.rs` `FnRow` and render, and if rolled up per file to
`rollup.rs` `FileMetrics` and the `summary.rs` group; document in
help/metrics.txt / summary.txt. Scored: add a `Measure` variant in
measure.rs (every match there, `Measure::of`; `COUNT` sizes the
per-measure arrays in rollup.rs and smell.rs), its peak in `rollup.rs` and `smell::Input::peak`, the
dimension mapping in `smell::rank`, `fnmetrics::arm_metrics` if arms
carry it, the help text phrase and `help_states_the_thresholds`, the culprit wording
in lua/mbloaty/text.lua (`culprits`), the tables at the top of
tests/invariants.rs; bump both SCHEMAs. Tests that tell you: fnmetrics
units, smell units, golden (expect a diff: explain it), invariants, nvim.

**Add a finding kind.** New `src/findings/<kind>.rs` with
`pub(super) fn <kind>(cx: &mut Cx, out: &mut Vec<Finding>)` and its tests
(helpers from `crate::findings::tests`); a `Kind` variant in
findings/mod.rs (declaration order is report order; serde snake_case,
clap alias), the call in `findings()`, the title in `render`; new
`Finding` fields only as `Option`s skipped when `None` (a field Lua reads
bumps the schema). Help: help/findings.txt. Fixture: a case in
tests/fixtures/ws and the kind in golden.rs `findings`; the `KINDS` list in
tests/invariants.rs. Lens: `MARK_KINDS` in lua/mbloaty/init.lua and
`finding_note` in text.lua if it should be marked in buffers.

**Add a shape.** Detect it in `shape.rs` (`Shape` variant, `shape()`,
`settle` if it needs code lines), say what it excuses in `rollup.rs`
(`Excuse` variant, `covers`, `effective_metrics`), its text in
`smell::Reason::text`, its row in `metrics.rs` (`ShapeRow`, text tag), help
in summary.txt / metrics.txt, its words in lua/mbloaty/text.lua (`excuse`,
`shape`), the excuse check in tests/invariants.rs `check_dim`; bump both
SCHEMAs. Add a fixture file whose role golden.rs asserts (mind the caps).

**Add a duplicate rule.** Only for a fact a group can be sorted by in
one step (no reasoning about what could call what). Add a `Rule` variant
in findings/duplicate/policy.rs, its row in `TABLE` (order matters: first
match wins), its `tier`, `text` and `hint` arms, and its case in
`example` (no wildcard: it won't compile without a case and a near miss;
the table test runs both). If it needs a new fact, add it to `Facts`
(computed in duplicate.rs `duplicate_finding`). Add it to `RULES` in
tests/invariants.rs with its tier, and to help/findings.txt. A new
`rule` value is a JSON change Lua may read: bump both SCHEMAs.

**Change display wording.** CLI text: the `render` fns and
`Measure::text` / `Reason::text` (the `why`); goldens contain `why` and
`detail`, so expect a golden diff. Neovim wording: lua/mbloaty/text.lua
only (the lens and tree call it); nvim_test.lua matches lens texts, so
update it alongside. Wording never changes JSON structure or schema.

## History

plan.md (gitignored) is the design log: every decision with the case that
drove it, step by step. Distilled rationale:

- Complexity is cognitive complexity (SonarSource's, adapted): a `match`
  counts once, `?` nothing, nesting costs; flat dispatch, guard clauses
  and error plumbing are not complexity. It replaced max(cyclomatic,
  nesting from 4), which flagged flat validation (cc 11, readable) and
  14-line fns at nesting 4, and missed tangled loops (comparison in
  plan.md, "Cognitive complexity phase 2").
- Every dot has a culprit: a level the lens can't point at (a big file of
  40–59-line straight fns) isn't actionable.
- Fragility ranks only files with ≥ 2 dense panics: otherwise one `unwrap`
  in a project that rarely panics ranks in the top decile.
- Population is the whole workspace so a file doesn't change colour with
  the directory the tree is opened at.
- Tables and dispatches are width, not difficulty: excused, but their
  heavy arms, lets and scrutinees still count, and the file is noted so
  nothing hides silently.
- `assert!` family counts as panics (they panic in release); `debug_assert!`
  doesn't.
- Macros are opaque (step 12): bloaty used to guess at expansions
  (`mod x;` in `cfg_if!` token streams, `impl .. for` in `macro_rules!`
  bodies, any macro's arguments as expressions). Each guess was a claim
  needing expansion; now only an allow-list of well-known expression
  macros is read, and what other macros name only suppresses claims.
- Duplicates are exact syntactic claims (the same signature and body),
  not judgements of what could share code: an earlier design reasoned
  about which copy could call which (visibility, crates, targets, cfg
  exclusivity) and grew into trees of heuristics; bloaty now states facts
  (crates, the dependency between them, cfgs, signatures) and leaves
  reachability to rust-analyzer. Equal bodies under different signatures
  (owned/borrowed pairs, typestates, one body on two types) and cfg
  variants are notes. A signature isn't extractable logic (areamy's 8-line
  `Get::get` signature around `Get::get(&self.input)` was a "duplicate"),
  so substance counts lines after it.
