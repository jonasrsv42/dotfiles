//! The hot file: one long, branchy, deeply nested fn.

pub struct Job {
    pub id: u32,
    pub deps: Vec<u32>,
    pub cost: u32,
    pub urgent: bool,
}

/// Orders jobs by a pile of ad-hoc rules.
pub fn plan(jobs: &[Job], budget: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut spent = 0;
    let mut skipped = 0;
    for job in jobs {
        if job.cost == 0 {
            continue;
        }
        if job.urgent && job.cost <= budget {
            out.push(job.id);
            spent += job.cost;
            continue;
        }
        match job.deps.len() {
            0 => {
                if spent + job.cost <= budget {
                    out.push(job.id);
                    spent += job.cost;
                } else {
                    skipped += 1;
                }
            }
            1 => {
                let dep = job.deps[0];
                if out.contains(&dep) {
                    if spent + job.cost <= budget {
                        out.push(job.id);
                        spent += job.cost;
                    } else if job.urgent || skipped > 3 {
                        out.insert(0, job.id);
                    }
                } else {
                    skipped += 1;
                }
            }
            n => {
                let mut ready = 0;
                for dep in &job.deps {
                    if out.contains(dep) {
                        ready += 1;
                    } else {
                        for other in jobs {
                            if other.id == *dep {
                                if other.cost == 0 || other.urgent {
                                    ready += 1;
                                }
                                break;
                            }
                        }
                    }
                }
                if ready == n && spent + job.cost <= budget {
                    out.push(job.id);
                    spent += job.cost;
                } else if ready * 2 >= n && !job.urgent {
                    skipped += 1;
                } else {
                    skipped += 2;
                }
            }
        }
        while spent > budget && !out.is_empty() {
            out.pop();
            spent = spent.saturating_sub(job.cost);
        }
    }
    if skipped > jobs.len() || out.is_empty() {
        out.extend(jobs.iter().filter(|j| j.urgent).map(|j| j.id));
    }
    out
}

pub fn total(jobs: &[Job]) -> u32 {
    jobs.iter().map(|j| j.cost).sum()
}
