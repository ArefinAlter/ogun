//! Token-bucket rate limits per client address and per API token (O-13 section 6.4).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::{ServerError, ServerResult};

/// What a bucket counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Every request.
    General,
    /// Sign-in, callback and enrolment.
    Auth,
    /// Edge sync.
    Sync,
    /// Requests with one API token.
    Token,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Ip(Class, IpAddr),
    Token(String),
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// The limiter.
#[derive(Debug)]
pub struct Limiter {
    buckets: Mutex<HashMap<Key, Bucket>>,
    per_minute: HashMap<Class, u32>,
}

/// IPv6 clients are limited per /64, the smallest block an ISP usually assigns.
fn bucket_ip(ip: IpAddr) -> IpAddr {
    match crate::config::canonical_ip(ip) {
        IpAddr::V6(v6) => {
            let bits = u128::from(v6) & (u128::MAX << 64);
            IpAddr::V6(bits.into())
        }
        v4 => v4,
    }
}

impl Limiter {
    /// A limiter with these per-minute rates.
    #[must_use]
    pub fn new(general: u32, auth: u32, sync: u32, token: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            per_minute: HashMap::from([
                (Class::General, general),
                (Class::Auth, auth),
                (Class::Sync, sync),
                (Class::Token, token),
            ]),
        }
    }

    fn take(&self, key: Key, class: Class) -> ServerResult<()> {
        let rate = f64::from(self.per_minute.get(&class).copied().unwrap_or(60).max(1));
        let now = Instant::now();
        let mut map = self
            .buckets
            .lock()
            .map_err(|_| ServerError::internal("rate limiter poisoned"))?;
        if map.len() > 100_000 {
            // Drop buckets idle for ten minutes; they are full again anyway.
            map.retain(|_, b| now.duration_since(b.last) < Duration::from_secs(600));
        }
        let b = map.entry(key).or_insert(Bucket {
            tokens: rate,
            last: now,
        });
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * rate / 60.0).min(rate);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - b.tokens) * 60.0 / rate).ceil();
            // Bounded above by 60 s for any rate of at least one per minute.
            Err(ServerError::rate_limited(wait.clamp(1.0, 60.0) as u64))
        }
    }

    /// Counts one request of `class` from an address.
    ///
    /// # Errors
    ///
    /// `server.rate_limited` when the bucket is empty.
    pub fn check_ip(&self, class: Class, ip: IpAddr) -> ServerResult<()> {
        self.take(Key::Ip(class, bucket_ip(ip)), class)
    }

    /// Counts one request with an API token.
    ///
    /// # Errors
    ///
    /// `server.rate_limited` when the bucket is empty.
    pub fn check_token(&self, token_id: &str) -> ServerResult<()> {
        self.take(Key::Token(token_id.to_owned()), Class::Token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_empty_and_report_a_wait() {
        let l = Limiter::new(3, 1, 1, 2);
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        for _ in 0..3 {
            l.check_ip(Class::General, ip).unwrap();
        }
        let e = l.check_ip(Class::General, ip).unwrap_err();
        assert_eq!(e.code(), "server.rate_limited");
        assert!(e.retry_after_s.unwrap() >= 1);
        // Another class and another address have their own buckets.
        l.check_ip(Class::Auth, ip).unwrap();
        l.check_ip(Class::General, "192.0.2.2".parse().unwrap())
            .unwrap();
        l.check_token("tok_a").unwrap();
        l.check_token("tok_a").unwrap();
        assert!(l.check_token("tok_a").is_err());
    }

    #[test]
    fn ipv6_is_limited_per_64() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2::ffff".parse().unwrap();
        assert_eq!(bucket_ip(a), bucket_ip(b));
    }
}
