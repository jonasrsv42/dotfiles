//! Throwaway on-disk workspaces for tests.

use crate::scan::{Scan, ScanOptions, scan};
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
