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
    out
}
