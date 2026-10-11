//! Dispatches: one match, measured by its worst arm. `run` is long because
//! it is wide, and four of its arms open with the same prelude; `weight` is
//! a small, clean one.

pub struct Req {
    pub id: String,
    pub name: String,
}

pub enum Cmd {
    Add(Req),
    Remove(Req),
    Rename(Req),
    Move(Req),
    List,
    Show(u32),
}

fn announce(log: &mut Vec<String>, id: &str) -> Result<u32, String> {
    log.push(format!("request {id}"));
    id.parse().map_err(|_| format!("bad id {id}"))
}

pub fn run(cmd: Cmd, log: &mut Vec<String>) -> Result<u32, String> {
    match cmd {
        Cmd::Add(r) => {
            let id = match announce(log, &r.id) {
                Ok(id) => id,
                Err(e) => return Err(e),
            };
            log.push(format!("add {}", r.name));
            Ok(id)
        }
        Cmd::Remove(r) => {
            let id = match announce(log, &r.id) {
                Ok(id) => id,
                Err(e) => return Err(e),
            };
            log.retain(|l| !l.ends_with(&r.name));
            Ok(id)
        }
        Cmd::Rename(r) => {
            let id = match announce(log, &r.id) {
                Ok(id) => id,
                Err(e) => return Err(e),
            };
            let old = log.pop().unwrap_or_default();
            log.push(format!("{old} -> {}", r.name));
            Ok(id)
        }
        Cmd::Move(r) => {
            let id = match announce(log, &r.id) {
                Ok(id) => id,
                Err(e) => return Err(e),
            };
            log.insert(0, format!("move {}", r.name));
            Ok(id + 1)
        }
        Cmd::List => Ok(log.len() as u32),
        Cmd::Show(n) => {
            let shown = log.iter().take(n as usize).count();
            Ok(shown as u32)
        }
    }
}

pub fn weight(cmd: &Cmd) -> usize {
    match cmd {
        Cmd::Add(r) => r.name.len() + 1,
        Cmd::Remove(r) => r.id.len() * 2,
        Cmd::Rename(r) => r.name.len() + r.id.len(),
        Cmd::Move(r) => r.name.len().saturating_sub(1),
        Cmd::List => 0,
        Cmd::Show(n) => *n as usize % 7,
    }
}
