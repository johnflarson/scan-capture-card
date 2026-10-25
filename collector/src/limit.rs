//! A small in-memory rate limit per address. Addresses live only here, for
//! as long as their window lasts.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Limiter {
    max: u32,
    window: Duration,
    seen: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Limiter {
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Count one more from `ip`; false when it is over the limit.
    pub fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        let entry = seen.entry(ip).or_insert((now, 0));
        entry.1 += 1;
        entry.1 <= self.max
    }

    /// Whether `ip` already used up its window (without counting).
    pub fn blocked(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.get(&ip)
            .is_some_and(|(start, n)| now.duration_since(*start) < self.window && *n >= self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_address_within_the_window() {
        let l = Limiter::new(2, Duration::from_secs(60));
        let a: IpAddr = "1.2.3.4".parse().unwrap();
        let b: IpAddr = "5.6.7.8".parse().unwrap();
        assert!(l.allow(a) && l.allow(a));
        assert!(l.blocked(a));
        assert!(!l.allow(a));
        assert!(!l.blocked(b) && l.allow(b));

        let short = Limiter::new(1, Duration::from_millis(1));
        assert!(short.allow(a));
        std::thread::sleep(Duration::from_millis(5));
        assert!(short.allow(a), "a new window starts");
    }
}
