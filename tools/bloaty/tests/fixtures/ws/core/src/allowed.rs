//! A long fn that opts out of the size check, the clippy way: noted, not
//! flagged.

pub struct Stats {
    pub done: u32,
    pub failed: u32,
    pub queued: u32,
    pub workers: u32,
}

#[cfg_attr(not(test), allow(clippy::too_many_lines))]
pub fn report(s: &Stats) -> Vec<String> {
    let mut out = Vec::new();
    out.push("== report ==".to_string());
    let total = s.done + s.failed + s.queued;
    out.push(format!("total: {total}"));
    out.push(format!("done: {}", s.done));
    out.push(format!("failed: {}", s.failed));
    out.push(format!("queued: {}", s.queued));
    out.push(format!("workers: {}", s.workers));
    let rate = s.done * 100 / total.max(1);
    out.push(format!("success: {rate}%"));
    let per_worker = total / s.workers.max(1);
    out.push(format!("per worker: {per_worker}"));
    let backlog = s.queued.saturating_sub(s.workers);
    out.push(format!("backlog: {backlog}"));
    let bar = "#".repeat(rate as usize / 10);
    out.push(format!("[{bar:<10}]"));
    let failed_bar = "x".repeat((s.failed * 10 / total.max(1)) as usize);
    out.push(format!("[{failed_bar:<10}]"));
    let eta_min = backlog * 3;
    out.push(format!("eta: {eta_min} min"));
    let eta_h = eta_min / 60;
    out.push(format!("eta: {eta_h} h"));
    let load = s.queued * 100 / s.workers.max(1);
    out.push(format!("load: {load}%"));
    let idle = s.workers.saturating_sub(s.queued);
    out.push(format!("idle workers: {idle}"));
    let ratio = s.failed * 1000 / s.done.max(1);
    out.push(format!("failure ratio: {ratio}‰"));
    let width = out.iter().map(String::len).max().unwrap_or(0);
    out.push("-".repeat(width));
    let summary = format!("{} of {} done", s.done, total);
    out.push(summary);
    let header = format!("{:^width$}", "jobs");
    out.insert(0, header);
    let footer = format!("{:>width$}", "end");
    out.push(footer);
    let lines = out.len();
    out.push(format!("({lines} lines)"));
    out.push(String::new());
    out.push(format!("slot 0: {}", s.done % 2));
    out.push(format!("slot 1: {}", s.done % 3));
    out.push(format!("slot 2: {}", s.done % 4));
    out.push(format!("slot 3: {}", s.done % 5));
    out.push(format!("slot 4: {}", s.done % 6));
    out.push(format!("slot 5: {}", s.done % 7));
    out.push(format!("slot 6: {}", s.done % 8));
    out.push(format!("slot 7: {}", s.done % 9));
    out.push(format!("slot 8: {}", s.done % 10));
    out.push(format!("slot 9: {}", s.done % 11));
    out.push(format!("slot 10: {}", s.done % 12));
    out.push(format!("slot 11: {}", s.done % 13));
    out.push(format!("slot 12: {}", s.done % 14));
    out.push(format!("slot 13: {}", s.done % 15));
    out.push(format!("slot 14: {}", s.done % 16));
    out.push(format!("slot 15: {}", s.done % 17));
    out.push(format!("slot 16: {}", s.done % 18));
    out.push(format!("slot 17: {}", s.done % 19));
    out.push(format!("slot 18: {}", s.done % 20));
    out.push(format!("slot 19: {}", s.done % 21));
    out
}
