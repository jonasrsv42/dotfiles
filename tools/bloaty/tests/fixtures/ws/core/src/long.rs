//! A long, straight-line fn, just under the bar (40 lines): ordinary.

pub struct Config {
    pub name: String,
    pub port: u16,
    pub host: String,
    pub retries: u32,
    pub verbose: bool,
}

pub fn describe(c: &Config) -> String {
    let mut s = String::new();
    let name = c.name.clone();
    s.push_str("config ");
    s.push_str(&name);
    s.push('\n');
    s.push_str("  host: ");
    s.push_str(&c.host);
    s.push('\n');
    s.push_str("  port: ");
    s.push_str(&c.port.to_string());
    s.push('\n');
    s.push_str("  retries: ");
    s.push_str(&c.retries.to_string());
    s.push('\n');
    s.push_str("  verbose: ");
    s.push_str(if c.verbose { "yes" } else { "no" });
    s.push('\n');
    let url = format!("http://{}:{}/", c.host, c.port);
    s.push_str("  url: ");
    s.push_str(&url);
    s.push('\n');
    let backoff: Vec<String> = (0..c.retries).map(|i| (100u32 << i).to_string()).collect();
    s.push_str("  backoff: ");
    s.push_str(&backoff.join(", "));
    s.push('\n');
    let width = c.name.len().max(c.host.len());
    s.push_str("  width: ");
    s.push_str(&width.to_string());
    s.push('\n');
    let banner = "=".repeat(width);
    s.push_str(&banner);
    s.push('\n');
    s.push_str(&banner);
    s
}
