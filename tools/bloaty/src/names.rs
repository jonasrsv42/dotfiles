//! How items are named and located in reports.

use crate::scan::{FileAnalysis, Scan};
use crate::visit::FnFact;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct Loc {
    pub name: String,
    /// Relative to the workspace root.
    pub file: String,
    /// Absolute.
    pub path: String,
    /// First line of the item's doc comments and attributes: delete from
    /// here to `end_line` to remove it whole.
    pub start_line: usize,
    /// The line with the item's name (where to jump).
    pub line: usize,
    pub end_line: usize,
    #[serde(rename = "crate")]
    pub krate: String,
}

impl Loc {
    pub fn new(scan: &Scan, f: &FileAnalysis, name: &str, lines: (usize, usize, usize)) -> Loc {
        let (start_line, line, end_line) = lines;
        Loc {
            name: name.to_string(),
            file: f.rel.clone(),
            path: f.path.display().to_string(),
            start_line,
            line,
            end_line,
            krate: scan.crate_name(f).to_string(),
        }
    }

    pub fn of_fn(scan: &Scan, f: &FileAnalysis, func: &FnFact) -> Loc {
        Loc::new(
            scan,
            f,
            &func.name,
            (func.start_line, func.line, func.end_line),
        )
    }
}

/// Long names with generics read `<Node<…> as GraphBuilder<…>>::build` in
/// text output; JSON keeps them whole.
pub fn short_name(name: &str) -> String {
    if name.chars().count() <= 60 {
        return name.to_string();
    }
    collapse_generics(name)
}

/// `<Node<A, B> as Build<C>>::f` -> `<Node<…> as Build<…>>::f`.
pub fn collapse_generics(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    let mut prev = ' ';
    for c in name.chars() {
        match c {
            // Generic arguments open right after a name: `Node<`, not `<Node`.
            '<' if depth > 0 || prev.is_alphanumeric() || prev == '_' => {
                if depth == 0 {
                    out.push_str("<…");
                }
                depth += 1;
            }
            '>' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push('>');
                }
            }
            _ if depth > 0 => {}
            c => out.push(c),
        }
        prev = c;
    }
    if depth > 0 {
        out.push('>');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_collapse_generics() {
        let long = "<Node<Allocated, Async, Async, Sync, Left, Right, Out> as GraphBuilder<ThreadIdType>>::build";
        assert_eq!(short_name(long), "<Node<…> as GraphBuilder<…>>::build");
        assert_eq!(short_name("Foo<T>::f"), "Foo<T>::f");
        let huge = format!(
            "<Wrapper<{}> as Tr>::never_called_method_name",
            "A, ".repeat(60)
        );
        assert_eq!(
            short_name(&huge),
            "<Wrapper<…> as Tr>::never_called_method_name"
        );
    }
}
