// Support code: measured, never scored.
fn main() {
    let profile = std::env::var("PROFILE").unwrap();
    let opt = std::env::var("OPT_LEVEL").unwrap();
    if profile == "release" {
        println!("cargo:rustc-cfg=fx_release");
    }
    for key in ["A", "B", "C", "D"] {
        if let Ok(v) = std::env::var(key) {
            assert!(!v.is_empty());
            println!("cargo:warning={key}={v}");
        }
    }
    println!("cargo:rustc-env=FX_OPT={opt}");
    println!("cargo:rerun-if-changed=build.rs");
}
