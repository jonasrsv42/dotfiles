//! `single_impl_trait`: workspace traits with at most one impl, trait paths
//! resolved through imports.

use super::{Category, Cx, Finding, Kind, Reason};
use crate::names::Loc;
use crate::scan::{FileAnalysis, Scan};
use crate::visit::{ImplKind, ItemKind, Vis};
use std::collections::{HashMap, HashSet};

/// Std derives that never implement a workspace trait unless imported.
const STD_DERIVES: &[&str] = &[
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
];

/// Resolves trait paths in impls and derives to (crate, trait name), using
/// the file's imports, so `fmt::Display` or `#[derive(Debug)]` don't count
/// as impls of a workspace trait that happens to share the name.
struct Resolver<'a> {
    crate_by_lib: HashMap<&'a str, usize>,
    modules: Vec<HashSet<&'a str>>,
    /// Workspace trait definitions: (crate, name).
    traits: HashSet<(usize, &'a str)>,
    /// Trait name -> crates defining it.
    trait_crates: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Resolver<'a> {
    fn new(scan: &'a Scan) -> Self {
        let crate_by_lib = scan
            .crates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.lib_name.as_str(), i))
            .collect();
        let mut modules = vec![HashSet::new(); scan.crates.len()];
        let mut traits = HashSet::new();
        let mut trait_crates: HashMap<&str, Vec<usize>> = HashMap::new();
        for f in &scan.files {
            let Some(c) = f.crate_idx else { continue };
            // A word in an opaque macro may be a module it declares
            // (`cfg_if! { .. mod sys; .. }`): paths through it stay ours.
            let declared = f.facts.modules.iter().chain(&f.facts.macro_words);
            modules[c].extend(declared.map(String::as_str));
            for t in f.facts.items.iter().filter(|i| i.kind == ItemKind::Trait) {
                if traits.insert((c, t.name.as_str())) {
                    trait_crates.entry(t.name.as_str()).or_default().push(c);
                }
            }
        }
        Resolver {
            crate_by_lib,
            modules,
            traits,
            trait_crates,
        }
    }

    fn crate_of_root(&self, this: usize, seg: &str) -> Option<usize> {
        match seg {
            "crate" | "self" | "super" => Some(this),
            s if self.modules[this].contains(s) => Some(this),
            s => self.crate_by_lib.get(s).copied(),
        }
    }

    /// `None` for traits from outside the workspace.
    fn resolve(&self, f: &FileAnalysis, path: &[String], derive: bool) -> Option<(usize, String)> {
        let this = f.crate_idx?;
        let (first, name) = (path.first()?, path.last()?);
        let import = f.facts.imports.iter().find(|(alias, _)| alias == first);
        let (c, name) = match (path.len(), import) {
            (1, Some((_, full))) => (self.crate_of_root(this, &full[0])?, full.last()?.clone()),
            (1, None) if derive && STD_DERIVES.contains(&name.as_str()) => return None,
            (1, None) => (this, name.clone()),
            (_, Some((_, full))) => (self.crate_of_root(this, &full[0])?, name.clone()),
            (_, None) => (self.crate_of_root(this, first)?, name.clone()),
        };
        if self.traits.contains(&(c, name.as_str())) {
            return Some((c, name));
        }
        // Reached through a glob import (`use dep::prelude::*`) or a
        // re-export in another crate: trust a name only one crate defines.
        match self.trait_crates.get(name.as_str()).map(Vec::as_slice) {
            Some([only]) => Some((*only, name)),
            _ => None,
        }
    }
}

/// Every workspace trait's impls (with the impl's bare self type name), and
/// the traits exempt from the check (blanket impls).
struct TraitImpls<'a> {
    impls: HashMap<(usize, String), Vec<(Loc, &'a str)>>,
    exempt: HashSet<(usize, String)>,
}

fn collect_trait_impls<'a>(cx: &Cx<'a>, resolver: &Resolver) -> TraitImpls<'a> {
    let mut t = TraitImpls {
        impls: HashMap::new(),
        exempt: HashSet::new(),
    };
    for f in &cx.scan.files {
        for i in f
            .facts
            .impls
            .iter()
            .filter(|i| i.kind != ImplKind::Inherent)
        {
            let derive = i.kind == ImplKind::Derive;
            let Some(key) = resolver.resolve(f, &i.trait_path, derive) else {
                continue;
            };
            match i.kind {
                ImplKind::Blanket => {
                    t.exempt.insert(key);
                }
                ImplKind::Concrete | ImplKind::Derive => {
                    let name = if derive {
                        format!("#[derive] {}", i.self_ty)
                    } else {
                        i.self_ty.clone()
                    };
                    let at = Loc::new(cx.scan, f, &name, (i.start_line, i.line, i.end_line));
                    t.impls.entry(key).or_default().push((at, &i.self_name));
                }
                ImplKind::Forwarding | ImplKind::Inherent => {}
            }
        }
    }
    t
}

pub(super) fn single_impl_traits(cx: &mut Cx, out: &mut Vec<Finding>) {
    let resolver = Resolver::new(cx.scan);
    let found = collect_trait_impls(cx, &resolver);
    let mut crate_types: HashMap<Option<usize>, HashSet<&str>> = HashMap::new();
    for f in &cx.scan.files {
        crate_types
            .entry(f.crate_idx)
            .or_default()
            .extend(f.facts.type_names.iter().map(String::as_str));
    }

    let mut trait_defs: HashMap<&str, u32> = HashMap::new();
    for f in &cx.scan.files {
        for t in f.facts.items.iter().filter(|t| t.kind == ItemKind::Trait) {
            *trait_defs.entry(t.name.as_str()).or_default() += 1;
        }
    }
    let traits: Vec<_> = cx
        .focus()
        .filter(|f| !f.whole_test)
        .flat_map(|f| f.facts.items.iter().map(move |t| (f, t)))
        .filter(|(_, t)| t.kind == ItemKind::Trait && !t.in_test)
        .collect();
    for (f, t) in traits {
        let Some(c) = f.crate_idx else { continue };
        let key = (c, t.name.clone());
        let impls = found.impls.get(&key).map(Vec::as_slice).unwrap_or(&[]);
        let (r, tr) = cx.refs(&t.name);
        let local = |ty: &str| {
            crate_types
                .get(&f.crate_idx)
                .is_some_and(|s| s.contains(ty))
        };
        let detail = match impls {
            _ if found.exempt.contains(&key) => continue,
            [] if r == 0 => continue, // reported as unused
            // An extension trait adding methods to a type from another
            // crate: the orphan rule makes the trait necessary.
            [(_, self_name)] if !local(self_name) => continue,
            [] if t.vis == Vis::Public => {
                "no impls in the workspace; pub, so maybe implemented elsewhere".to_string()
            }
            [] => format!("never implemented, but named {r} times"),
            [(one, _)] => format!("only impl: {}", one.name),
            _ => continue,
        };
        // Impls match traits by name: with two traits of one name, which
        // impl is whose takes name resolution. A macro naming the trait
        // may implement it any number of times.
        let same_name = trait_defs.get(t.name.as_str()).copied().unwrap_or(0) > 1;
        if same_name || cx.macro_words.contains(t.name.as_str()) {
            let why = if same_name {
                Reason::SameName
            } else {
                Reason::NamedInMacro
            };
            cx.exclude(Kind::SingleImplTrait, why);
            continue;
        }
        if cx.skip_pub(Kind::SingleImplTrait, f, t.vis) {
            continue;
        }
        let lines = f.code_lines(t.line, t.end_line);
        let at = Loc::new(cx.scan, f, &t.name, (t.start_line, t.line, t.end_line));
        out.push(
            Finding {
                vis: Some(t.vis),
                lines,
                refs: Some(r),
                test_refs: Some(tr),
                impls: Some(impls.iter().map(|(l, _)| l.clone()).collect()),
                detail,
                ..Finding::new(Kind::SingleImplTrait, Category::Impl, at)
            }
            .rank(0, lines),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::findings::tests::*;

    #[test]
    fn traits_resolve_across_crates() {
        let fx = Fixture::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"core\", \"app\", \"facade\"]\n",
            ),
            (
                "core/Cargo.toml",
                "[package]\nname = \"my-core-pkg\"\n[lib]\nname = \"mcore\"\n",
            ),
            (
                "core/src/lib.rs",
                "pub trait Codec {}\npub trait Plugin {}\npub trait Render {}\npub trait Display {}\npub trait Debug {}\npub trait Lonely {}\npub trait Ext {}\npub trait EpochExt {}\npub struct Core;\nimpl Codec for Core {}\nimpl Plugin for Core {}\nimpl Render for Core {}\nimpl Display for Core {}\nimpl Debug for Core {}\nimpl Lonely for Core {}\nimpl Ext for std::time::Duration {}\nimpl EpochExt for facade::Epoch {}\npub mod prelude {\n    pub use crate::Plugin;\n}\n",
            ),
            ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
            (
                "app/src/main.rs",
                "use mcore::prelude::*;\nuse std::fmt;\nstruct A;\nimpl mcore::Codec for A {}\nimpl Plugin for A {}\nimpl facade::Render for A {}\nimpl fmt::Display for A {}\n#[derive(Debug)]\nstruct B;\nfn main() {}\n",
            ),
            ("facade/Cargo.toml", "[package]\nname = \"facade\"\n"),
            (
                "facade/src/lib.rs",
                "pub use mcore::Render;\npub struct Epoch;\n",
            ),
        ]);
        let mut single = names(&run(&fx, true), Kind::SingleImplTrait);
        single.sort();
        assert_eq!(single, vec!["Debug", "Display", "Lonely"]);
    }

    #[test]
    fn shared_names_and_macro_named_traits_make_no_claim() {
        let all = unused_in(
            "mod a {\n    pub trait Codec {}\n}\nmod b {\n    pub trait Codec {}\n}\ntrait Gen {}\ntrait Solo {}\nstruct X;\nimpl a::Codec for X {}\nimpl Solo for X {}\nmacro_rules! g { ($t:ty) => { impl Gen for $t {} }; }\ng!(X);\nfn main() {}\n",
            &[],
        );
        assert_eq!(names(&all, Kind::SingleImplTrait), vec!["Solo"]);
        let why = &all.unclaimed[&Kind::SingleImplTrait];
        assert_eq!(why.get(&Reason::SameName), Some(&2));
        assert_eq!(why.get(&Reason::NamedInMacro), Some(&1));
    }
}
