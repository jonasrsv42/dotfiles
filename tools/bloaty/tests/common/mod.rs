//! Shared by the integration tests: a throwaway copy of the fixture
//! workspace and running the built binary.
//!
//! The fixture is copied to a temporary directory first: in place, the
//! repository's tests/fixtures/.bloatyignore hides it (so it doesn't skew
//! bloaty's view of itself), and with an empty `.git` of its own its root is
//! the fixture itself, whatever repository or Cargo.toml surrounds the temp
//! dir.

// Each test crate uses its own subset.
#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "target" || name == "Cargo.lock" {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(&name));
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// A fresh copy of the fixture, removed on drop.
pub struct Ws(pub PathBuf);

impl Ws {
    /// `tag` keeps the tests, which run in parallel, apart.
    pub fn new(tag: &str) -> Self {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = std::env::temp_dir().join(format!("bloaty-ws-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_dir(&manifest.join("tests/fixtures/ws"), &dir.join("ws"));
        // Its own repository: a git checkout or Cargo.toml around the temp
        // dir can't move the workspace root.
        std::fs::create_dir_all(dir.join("ws/.git")).unwrap();
        Ws(dir.join("ws").canonicalize().unwrap())
    }

    /// `bloaty args` in the copy, its JSON with the copy's path as `<ws>`.
    pub fn run(&self, args: &[&str]) -> Value {
        let stdout = run_raw(&self.0, args);
        let ws = self.0.to_str().unwrap();
        serde_json::from_str(&stdout.replace(ws, "<ws>")).unwrap()
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

/// `bloaty args` run in `dir`: its stdout. Panics (with stderr) unless it
/// exits 0.
pub fn run_raw(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_bloaty"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bloaty {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `bloaty args --json` run in `dir`, parsed.
pub fn run_json(dir: &Path, args: &[&str]) -> Value {
    let stdout = run_raw(dir, args);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("bloaty {args:?} in {}: bad JSON: {e}", dir.display()))
}
