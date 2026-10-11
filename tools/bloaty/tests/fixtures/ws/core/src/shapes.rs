//! A trait with a single impl, and an unused fn.

pub trait Area {
    fn area(&self) -> f64;
    fn scaled(&self, k: f64) -> f64 {
        self.area() * k * k
    }
}

pub struct Square(pub f64);

impl Area for Square {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}

pub fn largest(shapes: &[Square]) -> f64 {
    let mut best = 0.0;
    for s in shapes {
        if s.area() > best {
            best = s.area();
        }
    }
    best
}

pub fn never_called(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::MIN, f64::max)
}
