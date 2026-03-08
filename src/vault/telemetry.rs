//! # Vault Security Telemetry & Session Management
//!
//! Enterprise-grade secret access telemetry, rate limiting, vault sessions,
//! anomaly detection, and HMAC-based fingerprinting.
//!
//! All concurrent structures use DashMap for lock-free performance.
//! All sensitive buffers are zeroized after use.

use chrono::Utc;
use dashmap::DashMap;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroize;

// ---------------------------------------------------------------------------
// HKDF-derived fingerprint key
// ---------------------------------------------------------------------------

/// Derive a dedicated 32-byte fingerprint key from the vault master key using
/// HKDF-SHA256.  This avoids reusing the encryption key for HMAC operations.
pub fn derive_fingerprint_key(master_key: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master_key);
    let mut okm = [0u8; 32];
    // Context string separates the fingerprint key domain from encryption.
    hk.expand(b"vault-fingerprint", &mut okm)
        .expect("HKDF expand failed — output length is valid");
    okm
}

// ---------------------------------------------------------------------------
// HMAC fingerprinting
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

/// Compute HMAC-SHA256 fingerprint of `secret` using the dedicated
/// `fingerprint_key` (derived via HKDF, **not** the raw master key).
pub fn hmac_fingerprint(secret: &[u8], fingerprint_key: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(fingerprint_key).expect("HMAC can accept any key length");
    mac.update(secret);
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// Hex-encode helper (inline, avoids pulling in the `hex` crate).
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

/// Compute HMAC fingerprints for all alphanumeric tokens (≥4 chars) in a
/// secret value plus sliding 6-char windows.
pub fn hmac_fingerprint_tokens(secret: &str, fingerprint_key: &[u8]) -> Vec<String> {
    use std::collections::HashSet;
    let mut fingerprints = HashSet::new();

    for token in secret
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 4)
    {
        fingerprints.insert(hmac_fingerprint(token.as_bytes(), fingerprint_key));
    }

    let collapsed: String = secret
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if collapsed.len() >= 6 {
        let chars: Vec<char> = collapsed.chars().collect();
        let max_windows = chars.len().saturating_sub(6).min(128);
        for idx in 0..=max_windows {
            let end = (idx + 6).min(chars.len());
            let chunk: String = chars[idx..end].iter().collect();
            if chunk.len() >= 4 {
                fingerprints.insert(hmac_fingerprint(chunk.as_bytes(), fingerprint_key));
            }
        }
    }

    fingerprints.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Secret Access Event (telemetry)
// ---------------------------------------------------------------------------

/// Structured telemetry event for every secret resolution.
/// **Never** contains the secret value itself.
#[derive(Debug, Clone, Serialize)]
pub struct SecretAccessEvent {
    pub event: &'static str,
    pub timestamp: String,
    pub task_id: String,
    pub tool_name: String,
    pub secret_key: String,
    pub policy_result: String,
    pub executor_instance_id: String,
    pub policy_source: String,
}

impl SecretAccessEvent {
    pub fn new(
        task_id: &str,
        tool_name: &str,
        secret_key: &str,
        policy_result: &str,
        executor_instance_id: &str,
        policy_source: &str,
    ) -> Self {
        Self {
            event: "SECRET_ACCESS_EVENT",
            timestamp: Utc::now().to_rfc3339(),
            task_id: task_id.to_string(),
            tool_name: tool_name.to_string(),
            secret_key: secret_key.to_string(),
            policy_result: policy_result.to_string(),
            executor_instance_id: executor_instance_id.to_string(),
            policy_source: policy_source.to_string(),
        }
    }

    /// Emit this event to the `secret_telemetry` tracing target.
    pub fn emit(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            tracing::info!(target: "secret_telemetry", "{}", json);
        }
    }
}

// ---------------------------------------------------------------------------
// Rate Limiter (DashMap-backed, time-window)
// ---------------------------------------------------------------------------

struct RateEntry {
    counter: usize,
    window_start: Instant,
}

/// Lock-free rate limiter with per-task counters and a sliding time window.
pub struct SecretRateLimiter {
    max_per_task: usize,
    max_per_minute: usize,
    window_duration: Duration,
    entries: DashMap<String, RateEntry>,
    global_minute_counter: AtomicUsize,
    global_minute_start: std::sync::Mutex<Instant>,
}

impl SecretRateLimiter {
    pub fn new(max_per_task: usize, max_per_minute: usize) -> Self {
        Self {
            max_per_task,
            max_per_minute,
            window_duration: Duration::from_secs(60),
            entries: DashMap::new(),
            global_minute_counter: AtomicUsize::new(0),
            global_minute_start: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Check whether a secret resolution is allowed for `task_id`.
    /// Returns `Ok(())` if allowed, or an error string if the limit is exceeded.
    pub fn check_and_increment(&self, task_id: &str) -> Result<(), String> {
        // Per-task check
        let now = Instant::now();
        let mut entry = self
            .entries
            .entry(task_id.to_string())
            .or_insert(RateEntry {
                counter: 0,
                window_start: now,
            });

        // Reset window if expired
        if now.duration_since(entry.window_start) > self.window_duration {
            entry.counter = 0;
            entry.window_start = now;
        }

        if entry.counter >= self.max_per_task {
            return Err(format!(
                "secret_rate_limit_exceeded: task '{}' exceeded {} resolutions per window",
                task_id, self.max_per_task
            ));
        }
        entry.counter += 1;

        // Global per-minute check
        {
            let mut start = self.global_minute_start.lock().unwrap();
            if now.duration_since(*start) > self.window_duration {
                self.global_minute_counter.store(0, Ordering::SeqCst);
                *start = now;
            }
        }
        let global_count = self.global_minute_counter.fetch_add(1, Ordering::SeqCst);
        if global_count >= self.max_per_minute {
            return Err(format!(
                "secret_rate_limit_exceeded: global limit of {} resolutions per minute exceeded",
                self.max_per_minute
            ));
        }

        Ok(())
    }

    /// Reset all counters (used in tests).
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.entries.clear();
        self.global_minute_counter.store(0, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Vault Auto-Lock Session
// ---------------------------------------------------------------------------

/// Time-limited vault unlock session.  Secrets cannot be resolved when locked.
pub struct VaultSession {
    locked: bool,
    unlock_time: Option<Instant>,
    session_expiry: Option<Instant>,
    session_duration: Duration,
}

impl VaultSession {
    pub fn new(session_duration_secs: u64) -> Self {
        Self {
            locked: true,
            unlock_time: None,
            session_expiry: None,
            session_duration: Duration::from_secs(session_duration_secs),
        }
    }

    pub fn unlock(&mut self) {
        let now = Instant::now();
        self.locked = false;
        self.unlock_time = Some(now);
        self.session_expiry = Some(now + self.session_duration);
    }

    pub fn lock(&mut self) {
        self.locked = true;
        self.unlock_time = None;
        self.session_expiry = None;
    }

    /// Returns `true` if the vault session is unlocked and has not expired.
    pub fn is_active(&self) -> bool {
        if self.locked {
            return false;
        }
        match self.session_expiry {
            Some(expiry) => Instant::now() < expiry,
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Anomaly Detection Engine
// ---------------------------------------------------------------------------

/// Lightweight anomaly detector for secret access patterns.
pub struct AnomalyDetector {
    secrets_per_task: DashMap<String, usize>,
    secrets_per_tool: DashMap<String, usize>,
    failed_attempts: AtomicUsize,
    policy_violations: AtomicUsize,
    alert_threshold: usize,
}

impl AnomalyDetector {
    pub fn new(alert_threshold: usize) -> Self {
        Self {
            secrets_per_task: DashMap::new(),
            secrets_per_tool: DashMap::new(),
            failed_attempts: AtomicUsize::new(0),
            policy_violations: AtomicUsize::new(0),
            alert_threshold,
        }
    }

    pub fn record_resolution(&self, task_id: &str, tool_name: &str) {
        *self
            .secrets_per_task
            .entry(task_id.to_string())
            .or_insert(0) += 1;
        *self
            .secrets_per_tool
            .entry(tool_name.to_string())
            .or_insert(0) += 1;

        // Check per-task threshold
        if let Some(count) = self.secrets_per_task.get(task_id) {
            if *count > self.alert_threshold {
                tracing::warn!(
                    target: "secret_telemetry",
                    "SECURITY_ALERT excessive_secret_requests task_id={}",
                    task_id
                );
            }
        }

        // Check per-tool threshold
        if let Some(count) = self.secrets_per_tool.get(tool_name) {
            if *count > self.alert_threshold {
                tracing::warn!(
                    target: "secret_telemetry",
                    "SECURITY_ALERT unexpected_tool_secret_usage tool={}",
                    tool_name
                );
            }
        }
    }

    pub fn record_failed_attempt(&self) {
        let count = self.failed_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if count > self.alert_threshold {
            tracing::warn!(
                target: "secret_telemetry",
                "SECURITY_ALERT excessive_failed_secret_access_attempts count={}",
                count
            );
        }
    }

    pub fn record_policy_violation(&self) {
        let count = self.policy_violations.fetch_add(1, Ordering::SeqCst) + 1;
        if count > self.alert_threshold {
            tracing::warn!(
                target: "secret_telemetry",
                "SECURITY_ALERT excessive_policy_violations count={}",
                count
            );
        }
    }

    pub fn failed_attempt_count(&self) -> usize {
        self.failed_attempts.load(Ordering::SeqCst)
    }

    pub fn policy_violation_count(&self) -> usize {
        self.policy_violations.load(Ordering::SeqCst)
    }

    /// Returns `true` if any metric has exceeded the alert threshold.
    pub fn has_active_alert(&self) -> bool {
        if self.failed_attempts.load(Ordering::SeqCst) > self.alert_threshold {
            return true;
        }
        if self.policy_violations.load(Ordering::SeqCst) > self.alert_threshold {
            return true;
        }
        for entry in self.secrets_per_task.iter() {
            if *entry.value() > self.alert_threshold {
                return true;
            }
        }
        for entry in self.secrets_per_tool.iter() {
            if *entry.value() > self.alert_threshold {
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Secure memory helpers
// ---------------------------------------------------------------------------

/// Zeroize a `Vec<u8>` buffer and clear it.
pub fn secure_wipe_bytes(buf: &mut Vec<u8>) {
    buf.zeroize();
}

/// Zeroize a `String` buffer and clear it.
pub fn secure_wipe_string(s: &mut String) {
    // Safety: zeroize the underlying bytes then clear the string.
    unsafe {
        s.as_bytes_mut().zeroize();
    }
    s.clear();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hkdf_derives_distinct_key() {
        let master_key = b"test-master-password-32-bytes!!!";
        let fp_key = derive_fingerprint_key(master_key);
        // The fingerprint key must differ from the master key.
        assert_ne!(fp_key.as_slice(), master_key.as_slice());
        assert_eq!(fp_key.len(), 32);
    }

    #[test]
    fn test_hmac_fingerprint_deterministic() {
        let key = derive_fingerprint_key(b"deterministic-test-key-32bytes!");
        let fp1 = hmac_fingerprint(b"my_secret_value", &key);
        let fp2 = hmac_fingerprint(b"my_secret_value", &key);
        assert_eq!(fp1, fp2);
        // Different secret → different fingerprint.
        let fp3 = hmac_fingerprint(b"another_secret", &key);
        assert_ne!(fp1, fp3);
    }

    #[test]
    fn test_hmac_fingerprint_tokens_non_empty() {
        let key = derive_fingerprint_key(b"token-test-key-needs-32-bytes!!");
        let tokens = hmac_fingerprint_tokens("sk_live_123456", &key);
        assert!(!tokens.is_empty());
    }

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let limiter = SecretRateLimiter::new(3, 100);
        assert!(limiter.check_and_increment("task_1").is_ok());
        assert!(limiter.check_and_increment("task_1").is_ok());
        assert!(limiter.check_and_increment("task_1").is_ok());
    }

    #[test]
    fn test_rate_limiter_blocks_over_limit() {
        let limiter = SecretRateLimiter::new(2, 100);
        assert!(limiter.check_and_increment("task_1").is_ok());
        assert!(limiter.check_and_increment("task_1").is_ok());
        let result = limiter.check_and_increment("task_1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("secret_rate_limit_exceeded"));
    }

    #[test]
    fn test_rate_limiter_separate_tasks() {
        let limiter = SecretRateLimiter::new(1, 100);
        assert!(limiter.check_and_increment("task_a").is_ok());
        assert!(limiter.check_and_increment("task_b").is_ok());
        // task_a is now exhausted
        assert!(limiter.check_and_increment("task_a").is_err());
    }

    #[test]
    fn test_vault_session_active_after_unlock() {
        let mut session = VaultSession::new(600);
        assert!(!session.is_active());
        session.unlock();
        assert!(session.is_active());
    }

    #[test]
    fn test_vault_session_locked() {
        let mut session = VaultSession::new(600);
        session.unlock();
        assert!(session.is_active());
        session.lock();
        assert!(!session.is_active());
    }

    #[test]
    fn test_vault_session_expired() {
        // Create a session with 0-second duration so it expires immediately.
        let mut session = VaultSession::new(0);
        session.unlock();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(!session.is_active());
    }

    #[test]
    fn test_anomaly_detector_below_threshold() {
        let detector = AnomalyDetector::new(5);
        detector.record_resolution("t1", "http");
        detector.record_resolution("t1", "http");
        assert!(!detector.has_active_alert());
    }

    #[test]
    fn test_anomaly_detector_triggers_alert() {
        let detector = AnomalyDetector::new(2);
        detector.record_resolution("t1", "http");
        detector.record_resolution("t1", "http");
        detector.record_resolution("t1", "http"); // exceeds threshold of 2
        assert!(detector.has_active_alert());
    }

    #[test]
    fn test_anomaly_detector_per_tool_tracking() {
        let detector = AnomalyDetector::new(1);
        detector.record_resolution("t1", "shell");
        detector.record_resolution("t2", "shell"); // shell now at 2, threshold=1
        assert!(detector.has_active_alert());
    }

    #[test]
    fn test_secret_access_event_no_value_field() {
        let event = SecretAccessEvent::new(
            "task_1",
            "http",
            "api_key",
            "allowed",
            "executor-1",
            "config_policy",
        );
        let json = serde_json::to_string(&event).unwrap();
        // Must not contain a "value" field at all.
        assert!(!json.contains("\"value\""));
        // Must contain expected fields.
        assert!(json.contains("SECRET_ACCESS_EVENT"));
        assert!(json.contains("executor_instance_id"));
        assert!(json.contains("policy_source"));
    }

    #[test]
    fn test_secure_wipe_bytes() {
        let mut buf = vec![0xAA; 32];
        secure_wipe_bytes(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_secure_wipe_string() {
        let mut s = String::from("super_secret_value");
        secure_wipe_string(&mut s);
        assert!(s.is_empty());
    }
}
