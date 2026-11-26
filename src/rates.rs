//! Live currency exchange rates.
//!
//! Rates come from the free open.er-api.com endpoint (no API key) and are
//! cached on disk so the last known rates work offline. Until rates load, the
//! engine falls back to the approximate rates built into the unit table.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::engine::units::live_rates;

const RATES_URL: &str = "https://open.er-api.com/v6/latest/USD";
/// Refetch when the cache is older than this.
pub const MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

pub fn cache_path() -> PathBuf {
    crate::data_dir().join("rates.json")
}

/// Parses an API response and installs the rates. Returns a short label
/// describing when the rates were published (e.g. "26 Nov 2026").
pub fn apply(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("result")?.as_str()? != "success" {
        return None;
    }
    let rates = value.get("rates")?.as_object()?;
    let mut parsed = std::collections::HashMap::new();
    for (code, per_usd) in rates {
        // The API gives units of currency per 1 USD; we store USD per unit.
        if let Some(per_usd) = per_usd.as_f64()
            && per_usd > 0.0
        {
            parsed.insert(code.to_uppercase(), 1.0 / per_usd);
        }
    }
    if parsed.is_empty() {
        return None;
    }
    *live_rates().write().ok()? = parsed;
    let label = value
        .get("time_last_update_utc")
        .and_then(|t| t.as_str())
        // "Wed, 26 Nov 2026 00:02:31 +0000" -> "26 Nov 2026"
        .and_then(|t| t.get(5..16))
        .unwrap_or("today")
        .to_string();
    Some(label)
}

/// Loads cached rates. Returns the label and whether the cache is stale.
pub fn load_cache() -> Option<(String, bool)> {
    let path = cache_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let label = apply(&text)?;
    let stale = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age > MAX_AGE);
    Some((label, stale))
}

/// Downloads the latest rates (blocking; call from a background thread).
pub fn fetch() -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    agent
        .get(RATES_URL)
        .header("User-Agent", "Tallypad")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

pub fn save_cache(json: &str) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_response() {
        let json = r#"{"result":"success","time_last_update_utc":"Wed, 26 Nov 2026 00:02:31 +0000",
            "rates":{"USD":1,"EUR":0.5,"GBP":0.8}}"#;
        // Parse into a scratch map without touching the global used by other tests.
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["rates"]["EUR"].as_f64(), Some(0.5));
        assert!(apply(r#"{"result":"error"}"#).is_none());
        assert!(apply("not json").is_none());
    }
}
