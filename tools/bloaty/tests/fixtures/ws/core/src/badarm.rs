//! A dispatch with one bad arm: the arm is what counts, so the file stays
//! flagged, pointing at the arm.

pub enum Op {
    Push(u32),
    Pop,
    Clear,
    Rebalance { limit: u32, strict: bool },
}

pub fn apply(op: Op, stack: &mut Vec<u32>) -> u32 {
    match op {
        Op::Push(x) => {
            stack.push(x);
            x
        }
        Op::Pop => stack.pop().unwrap_or(0),
        Op::Clear => {
            stack.clear();
            0
        }
        Op::Rebalance { limit, strict } => {
            let mut moved = 0;
            for i in 0..stack.len() {
                if stack[i] > limit {
                    if strict && i > 0 {
                        stack[i - 1] += stack[i] - limit;
                        moved += 1;
                    } else if i + 1 < stack.len() {
                        let extra = stack[i] - limit;
                        if extra > 1 || !strict {
                            stack[i + 1] += extra;
                        }
                        moved += 1;
                    }
                    stack[i] = limit;
                } else if stack[i] == 0 && strict {
                    moved += 1;
                }
            }
            while stack.last() == Some(&0) {
                stack.pop();
            }
            moved
        }
    }
}
