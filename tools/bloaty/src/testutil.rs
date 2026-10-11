//! Throwaway on-disk workspaces for tests.

use crate::scan::{Scan, ScanOptions, scan};
use crate::visit::{Ctx, FileFacts, FnFact, collect};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Fixture {
    pub root: PathBuf,
}

impl Fixture {
    /// Writes `(relative path, contents)` pairs into a fresh directory.
    pub fn new(files: &[(&str, &str)]) -> Fixture {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("bloaty-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Its own repository, so a git checkout around the temp dir can't
        // change the workspace root.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        Fixture { root }
    }

    pub fn scan(&self) -> Scan {
        scan(std::slice::from_ref(&self.root), &ScanOptions::default()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The facts of `src` as a non-build-script file, with `features` treated
/// like cfg(test).
pub fn facts_with(src: &str, features: &[&str]) -> FileFacts {
    let test_features = features.iter().map(|s| s.to_string()).collect();
    let ctx = Ctx {
        test_features: &test_features,
        build_script: false,
    };
    collect(&syn::parse_file(src).unwrap(), &ctx)
}

pub fn facts(src: &str) -> FileFacts {
    facts_with(src, &[])
}

pub fn fact<'a>(f: &'a FileFacts, name: &str) -> &'a FnFact {
    f.fns
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no fn {name}"))
}
