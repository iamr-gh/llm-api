use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An explicit Codex subscription quota rejection, not a generic HTTP 429.
#[derive(Debug)]
pub struct CodexQuota {
    reset_after: Option<Duration>,
}

impl CodexQuota {
    /// Delay reported by the provider, measured when the rejection was received.
    pub fn reset_after(&self) -> Option<Duration> {
        self.reset_after
    }

    pub(super) fn from_error(payload: &Value, retry_after: Option<u64>) -> Option<Self> {
        let error = [&payload["response"]["error"], &payload["error"], payload]
            .into_iter()
            .find(|error| {
                [&error["type"], &error["code"]]
                    .into_iter()
                    .any(|code| code == "usage_limit_reached")
            })?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok();
        let plausible = |seconds: &u64| (1..=366 * 24 * 3600).contains(seconds);
        let reset_after = error["resets_at"]
            .as_u64()
            .and_then(|timestamp| timestamp.checked_sub(now?.as_secs()))
            .filter(plausible)
            .or_else(|| error["resets_in_seconds"].as_u64().filter(plausible))
            .or_else(|| retry_after.filter(plausible))
            .map(Duration::from_secs);
        Some(Self { reset_after })
    }
}

impl std::fmt::Display for CodexQuota {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Codex subscription usage limit reached")?;
        if let Some(delay) = self.reset_after {
            write!(
                f,
                " (provider reports a reset in {} seconds)",
                delay.as_secs()
            )?;
        }
        Ok(())
    }
}
impl std::error::Error for CodexQuota {}
