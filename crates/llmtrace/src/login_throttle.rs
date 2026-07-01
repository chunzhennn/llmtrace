use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::LoginRateLimitConfig;

const MAX_LOGIN_THROTTLE_USERNAME_BYTES: usize = 320;

#[derive(Debug, Clone)]
pub struct LoginThrottle {
    config: LoginRateLimitConfig,
    inner: Arc<Mutex<HashMap<LoginThrottleKey, LoginThrottleState>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginThrottleDecision {
    Allowed,
    Limited { retry_after: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LoginThrottleKey {
    username: String,
    remote_addr: String,
}

#[derive(Debug, Clone)]
struct LoginThrottleState {
    failures: u32,
    first_failure: Instant,
    locked_until: Option<Instant>,
    last_seen: Instant,
}

impl LoginThrottle {
    pub fn new(config: &LoginRateLimitConfig) -> Self {
        Self {
            config: config.clone(),
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn check(&self, username: &str, remote_addr: &str) -> LoginThrottleDecision {
        self.check_at(username, remote_addr, Instant::now())
    }

    pub fn record_failure(&self, username: &str, remote_addr: &str) -> Option<Duration> {
        self.record_failure_at(username, remote_addr, Instant::now())
    }

    pub fn record_success(&self, username: &str, remote_addr: &str) {
        self.record_success_at(username, remote_addr);
    }

    fn check_at(&self, username: &str, remote_addr: &str, now: Instant) -> LoginThrottleDecision {
        if !self.enabled() {
            return LoginThrottleDecision::Allowed;
        }

        let key = LoginThrottleKey::new(username, remote_addr);
        let mut attempts = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        prune_expired(&mut attempts, &self.config, now);

        let Some(state) = attempts.get(&key) else {
            return LoginThrottleDecision::Allowed;
        };
        limited_until(state.locked_until, now)
            .map(|retry_after| LoginThrottleDecision::Limited { retry_after })
            .unwrap_or(LoginThrottleDecision::Allowed)
    }

    fn record_failure_at(
        &self,
        username: &str,
        remote_addr: &str,
        now: Instant,
    ) -> Option<Duration> {
        if !self.enabled() {
            return None;
        }

        let key = LoginThrottleKey::new(username, remote_addr);
        let mut attempts = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        prune_expired(&mut attempts, &self.config, now);
        if !attempts.contains_key(&key) && attempts.len() >= self.config.max_tracked_entries {
            remove_oldest(&mut attempts);
        }

        let state = attempts.entry(key).or_insert_with(|| LoginThrottleState {
            failures: 0,
            first_failure: now,
            locked_until: None,
            last_seen: now,
        });

        if let Some(retry_after) = limited_until(state.locked_until, now) {
            state.last_seen = now;
            return Some(retry_after);
        }

        if now.duration_since(state.first_failure) > Duration::from_secs(self.config.window_secs) {
            state.failures = 0;
            state.first_failure = now;
            state.locked_until = None;
        }

        state.failures = state.failures.saturating_add(1);
        state.last_seen = now;

        if state.failures >= self.config.max_failures {
            let locked_until = now + Duration::from_secs(self.config.lockout_secs);
            state.locked_until = Some(locked_until);
            return limited_until(state.locked_until, now);
        }

        None
    }

    fn record_success_at(&self, username: &str, remote_addr: &str) {
        if !self.enabled() {
            return;
        }

        let key = LoginThrottleKey::new(username, remote_addr);
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&key);
    }

    fn enabled(&self) -> bool {
        self.config.enabled
            && self.config.max_failures > 0
            && self.config.window_secs > 0
            && self.config.lockout_secs > 0
            && self.config.max_tracked_entries > 0
    }
}

impl LoginThrottleKey {
    fn new(username: &str, remote_addr: &str) -> Self {
        Self {
            username: truncate_utf8(
                &username.trim().to_ascii_lowercase(),
                MAX_LOGIN_THROTTLE_USERNAME_BYTES,
            ),
            remote_addr: remote_addr.trim().to_string(),
        }
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn limited_until(locked_until: Option<Instant>, now: Instant) -> Option<Duration> {
    let locked_until = locked_until?;
    if locked_until <= now {
        return None;
    }
    let retry_after = locked_until.duration_since(now);
    Some(retry_after.max(Duration::from_secs(1)))
}

fn prune_expired(
    attempts: &mut HashMap<LoginThrottleKey, LoginThrottleState>,
    config: &LoginRateLimitConfig,
    now: Instant,
) {
    let window = Duration::from_secs(config.window_secs);
    attempts.retain(|_, state| {
        limited_until(state.locked_until, now).is_some()
            || now.duration_since(state.first_failure) <= window
    });
}

fn remove_oldest(attempts: &mut HashMap<LoginThrottleKey, LoginThrottleState>) {
    if let Some(key) = attempts
        .iter()
        .min_by_key(|(_, state)| state.last_seen)
        .map(|(key, _)| key.clone())
    {
        attempts.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_throttle() -> LoginThrottle {
        LoginThrottle::new(&LoginRateLimitConfig {
            enabled: true,
            max_failures: 3,
            window_secs: 60,
            lockout_secs: 120,
            max_tracked_entries: 16,
        })
    }

    #[test]
    fn locks_after_configured_failure_count() {
        let throttle = test_throttle();
        let now = Instant::now();

        assert!(
            throttle
                .record_failure_at("admin", "127.0.0.1", now)
                .is_none()
        );
        assert!(
            throttle
                .record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(1))
                .is_none()
        );
        let retry_after =
            throttle.record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(2));

        assert_eq!(retry_after, Some(Duration::from_secs(120)));
        assert_eq!(
            throttle.check_at("admin", "127.0.0.1", now + Duration::from_secs(3)),
            LoginThrottleDecision::Limited {
                retry_after: Duration::from_secs(119)
            }
        );
    }

    #[test]
    fn allows_after_lockout_expires() {
        let throttle = test_throttle();
        let now = Instant::now();

        for offset in 0..3 {
            throttle.record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(offset));
        }

        assert_eq!(
            throttle.check_at("admin", "127.0.0.1", now + Duration::from_secs(123)),
            LoginThrottleDecision::Allowed
        );
    }

    #[test]
    fn success_clears_failures_for_key() {
        let throttle = test_throttle();
        let now = Instant::now();

        throttle.record_failure_at("admin", "127.0.0.1", now);
        throttle.record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(1));
        throttle.record_success_at("admin", "127.0.0.1");

        assert!(
            throttle
                .record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(2))
                .is_none()
        );
    }

    #[test]
    fn remote_addresses_are_limited_independently() {
        let throttle = test_throttle();
        let now = Instant::now();

        for offset in 0..3 {
            throttle.record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(offset));
        }

        assert_eq!(
            throttle.check_at("admin", "127.0.0.2", now + Duration::from_secs(3)),
            LoginThrottleDecision::Allowed
        );
    }

    #[test]
    fn disabled_throttle_allows_requests() {
        let throttle = LoginThrottle::new(&LoginRateLimitConfig {
            enabled: false,
            ..LoginRateLimitConfig::default()
        });
        let now = Instant::now();

        for offset in 0..10 {
            throttle.record_failure_at("admin", "127.0.0.1", now + Duration::from_secs(offset));
        }

        assert_eq!(
            throttle.check_at("admin", "127.0.0.1", now + Duration::from_secs(10)),
            LoginThrottleDecision::Allowed
        );
    }

    #[test]
    fn throttle_key_bounds_username_length() {
        let key = LoginThrottleKey::new(&"a".repeat(MAX_LOGIN_THROTTLE_USERNAME_BYTES + 1), "ip");

        assert_eq!(key.username.len(), MAX_LOGIN_THROTTLE_USERNAME_BYTES);
    }

    #[test]
    fn throttle_key_truncates_on_utf8_boundary() {
        let username = format!("{}é", "a".repeat(MAX_LOGIN_THROTTLE_USERNAME_BYTES - 1));
        let key = LoginThrottleKey::new(&username, "ip");

        assert_eq!(key.username.len(), MAX_LOGIN_THROTTLE_USERNAME_BYTES - 1);
        assert!(key.username.is_char_boundary(key.username.len()));
    }
}
