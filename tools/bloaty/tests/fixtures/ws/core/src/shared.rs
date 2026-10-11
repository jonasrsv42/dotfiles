//! Declared twice in lib.rs: from test code and from impl code.

pub fn label(id: u32) -> String {
    let base = format!("job-{id}");
    let copy = base.clone();
    copy + "!"
}
