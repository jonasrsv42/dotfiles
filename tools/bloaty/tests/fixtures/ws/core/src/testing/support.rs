// Test support behind a #[path]: test code, with a child module.
mod data;

pub fn sample() -> Vec<u32> {
    data::NUMBERS.to_vec()
}
