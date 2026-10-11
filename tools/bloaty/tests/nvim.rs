//! The Neovim side (lua/mbloaty/) against this build: runs
//! nvim_test.lua headless when `nvim` is on PATH, and skips (saying so)
//! otherwise. The binary under test goes in as `$BLOATY`, so the script
//! doesn't run cargo (which would wait on this build's lock).

use std::process::Command;

#[test]
fn neovim_side() {
    let has_nvim = Command::new("nvim")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_nvim {
        eprintln!("skipping the Neovim test: no `nvim` on PATH");
        return;
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/nvim_test.lua");
    let out = Command::new("nvim")
        .args(["--headless", "-u", "NONE", "-l", script])
        .env("BLOATY", env!("CARGO_BIN_EXE_bloaty"))
        .output()
        .expect("nvim runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nvim_test.lua failed:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("checks passed"), "{stdout}\n{stderr}");
}
