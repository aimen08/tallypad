//! "Ask Claude" lines: a line starting with `?` is sent to Claude, which
//! answers with a Tallypad expression that the engine then evaluates itself.
//! Answers are cached on disk, so a sheet only costs a request once per
//! question. The API key lives in Windows Credential Manager.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value as Json, json};

use crate::data_dir;

/// (model id, short label, full name)
pub const MODELS: [(&str, &str, &str); 3] = [
    ("claude-opus-5-5", "Opus", "Claude Opus 5.5"),
    ("claude-sonnet-5-5", "Sonnet", "Claude Sonnet 5.5"),
    ("claude-haiku-5-5", "Haiku", "Claude Haiku 5.5"),
];
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const CREDENTIAL_TARGET: &str = "Tallypad/anthropic-api-key";

#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// Tallypad expression that computes the answer ("" when not numeric).
    pub expression: String,
    /// Short human answer, shown when there's no expression.
    pub answer: String,
}

pub fn short_name(model: &str) -> &'static str {
    MODELS
        .iter()
        .find(|m| m.0 == model)
        .map(|m| m.1)
        .unwrap_or("Claude")
}

/// Questions are cached by their normalised text.
pub fn cache_key(question: &str) -> String {
    question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// ---------------------------------------------------------------- settings ---

fn settings_path() -> std::path::PathBuf {
    data_dir().join("settings.json")
}

pub fn load_model() -> String {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Json>(&s).ok())
        .and_then(|j| j["model"].as_str().map(str::to_string))
        .filter(|m| MODELS.iter().any(|x| x.0 == m))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

pub fn save_model(model: &str) {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(settings_path(), json!({ "model": model }).to_string());
}

// ------------------------------------------------------------------- cache ---

fn cache_path() -> std::path::PathBuf {
    data_dir().join("ai-answers.json")
}

pub fn load_cache() -> HashMap<String, Answer> {
    let Some(j) = std::fs::read_to_string(cache_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Json>(&s).ok())
    else {
        return HashMap::new();
    };
    j.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    let field = |f: &str| v[f].as_str().unwrap_or_default().to_string();
                    (
                        k.clone(),
                        Answer {
                            expression: field("expression"),
                            answer: field("answer"),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_cache(cache: &HashMap<String, Answer>) {
    let map: serde_json::Map<String, Json> = cache
        .iter()
        .map(|(k, a)| {
            (
                k.clone(),
                json!({ "expression": a.expression, "answer": a.answer }),
            )
        })
        .collect();
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(cache_path(), Json::Object(map).to_string());
}

// ----------------------------------------------------------------- API key ---

/// `ANTHROPIC_API_KEY` wins; otherwise the key saved in Credential Manager.
pub fn api_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(credential::read)
}

pub fn has_api_key() -> bool {
    api_key().is_some()
}

pub fn set_api_key(key: &str) -> Result<(), String> {
    credential::write(key.trim())
}

pub fn remove_api_key() {
    credential::delete();
}

#[cfg(windows)]
mod credential {
    use super::CREDENTIAL_TARGET;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn read() -> Option<String> {
        let target = wide(CREDENTIAL_TARGET);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated; on success `cred` points to a
        // buffer owned by the system that we release with CredFree.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                return None;
            }
            let c = &*cred;
            let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let key = String::from_utf8(blob.to_vec()).ok();
            CredFree(cred as *const _);
            key.filter(|k| !k.is_empty())
        }
    }

    pub fn write(key: &str) -> Result<(), String> {
        let mut target = wide(CREDENTIAL_TARGET);
        let mut user = wide("Tallypad");
        let mut blob = key.as_bytes().to_vec();
        // SAFETY: every pointer in `cred` outlives the call.
        let ok = unsafe {
            let mut cred: CREDENTIALW = std::mem::zeroed();
            cred.Type = CRED_TYPE_GENERIC;
            cred.TargetName = target.as_mut_ptr();
            cred.UserName = user.as_mut_ptr();
            cred.CredentialBlobSize = blob.len() as u32;
            cred.CredentialBlob = blob.as_mut_ptr();
            cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
            CredWriteW(&cred, 0)
        };
        if ok == 0 {
            Err("couldn't save the key to Windows Credential Manager".into())
        } else {
            Ok(())
        }
    }

    pub fn delete() {
        let target = wide(CREDENTIAL_TARGET);
        // SAFETY: `target` is NUL-terminated.
        unsafe {
            CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0);
        }
    }
}

#[cfg(not(windows))]
mod credential {
    pub fn read() -> Option<String> {
        None
    }
    pub fn write(_: &str) -> Result<(), String> {
        Err("set ANTHROPIC_API_KEY instead".into())
    }
    pub fn delete() {}
}

// ----------------------------------------------------------------- request ---

const SYSTEM: &str = "You are the calculator assistant inside Tallypad, a notepad calculator. \
The user writes a question in any language. Reply with a Tallypad expression that computes the answer, \
so Tallypad does the arithmetic itself.

Tallypad expressions support:
- numbers: 1500, 1,500, 2.5k, 3M, 1.2bn, 0xFF
- operators: + - * / ^ ( ) and mod, factorial 5!
- percentages: 20% of 340, $80 - 25%, 10% off 250, 5% on 200, 15 as % of 60
- units, converted with 'in': 5 km in miles, 72 °F in °C, 2 GB in MB, 90 min in h, 60 km/h * 45 min, 3 m * 4 m
  (length, mass, time, data, temperature, area, volume, speed, angle)
- currencies by symbol or ISO code: $20, €15, £9, ¥1000, 100 USD in EUR, 250 EUR in JPY
- functions: sqrt cbrt abs round floor ceil sin cos tan asin acos atan log ln exp; constants pi and e
- variables already defined in the sheet, written exactly as they appear

Rules:
- Put the whole calculation in `expression`; don't compute the final number yourself unless it needs outside knowledge \
(then write the known quantity as a number with its unit, e.g. 299792458 m/s).
- Use only the syntax above. No words, no '=', no comments.
- If the answer isn't a number or quantity (a date, a name, yes/no), set `expression` to \"\".
- `answer` is always a very short answer for a person (at most 8 words), in the language of the question.";

fn api_base() -> String {
    std::env::var("TALLYPAD_API_BASE").unwrap_or_else(|_| "https://api.anthropic.com".into())
}

/// Builds the Messages API request body.
pub fn request_body(model: &str, question: &str, context: &str) -> Json {
    let user = if context.trim().is_empty() {
        format!("Question: {question}")
    } else {
        format!("The sheet so far (line = result):\n{context}\n\nQuestion: {question}")
    };
    let mut body = json!({
        "model": model,
        "max_tokens": 8000,
        "system": SYSTEM,
        "messages": [{ "role": "user", "content": user }],
        "output_config": {
            "effort": "low",
            "format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "expression": { "type": "string" },
                        "answer": { "type": "string" }
                    },
                    "required": ["expression", "answer"],
                    "additionalProperties": false
                }
            }
        }
    });
    if uses_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Server-side refusal fallbacks exist for Opus and Sonnet, not Haiku.
fn uses_fallbacks(model: &str) -> bool {
    !model.contains("haiku")
}

/// Reads the answer out of a Messages API response.
pub fn parse_response(body: &Json) -> Result<Answer, String> {
    match body["stop_reason"].as_str() {
        Some("refusal") => return Err("Claude declined this question".into()),
        Some("max_tokens") => return Err("the answer was cut off".into()),
        _ => {}
    }
    let text = body["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<String>();
    let parsed: Json = serde_json::from_str(text.trim())
        .map_err(|_| "unexpected reply from Claude".to_string())?;
    Ok(Answer {
        expression: parsed["expression"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
        answer: parsed["answer"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
}

/// Asks Claude (blocking; run it on a background thread).
pub fn ask(api_key: &str, model: &str, question: &str, context: &str) -> Result<Answer, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(90)))
            .http_status_as_error(false)
            .build(),
    );
    let mut req = agent
        .post(format!("{}/v1/messages", api_base()))
        .header("content-type", "application/json")
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION);
    if uses_fallbacks(model) {
        req = req.header("anthropic-beta", FALLBACK_BETA);
    }
    let mut res = req
        .send(request_body(model, question, context).to_string())
        .map_err(|e| format!("network error: {e}"))?;
    let status = res.status().as_u16();
    let text = res
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("network error: {e}"))?;
    let body: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
    if status != 200 {
        let msg = body["error"]["message"]
            .as_str()
            .unwrap_or("request failed");
        return Err(match status {
            401 => "invalid API key".to_string(),
            429 => "rate limited, try again shortly".to_string(),
            _ => format!("{status}: {msg}"),
        });
    }
    parse_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape() {
        let b = request_body(
            "claude-opus-5-5",
            "3 pizzas at €14 for 7",
            "rent = $1,200 = $1,200.00",
        );
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert!(
            b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("rent = $1,200")
        );
        assert!(b.get("thinking").is_none());
        let h = request_body("claude-haiku-5-5", "q", "");
        assert!(h.get("fallbacks").is_none());
    }

    #[test]
    fn parses_answers_and_refusals() {
        let ok = json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "{\"expression\":\"€14 * 3 / 7\",\"answer\":\"€6 each\"}" }
            ]
        });
        assert_eq!(
            parse_response(&ok).unwrap(),
            Answer {
                expression: "€14 * 3 / 7".into(),
                answer: "€6 each".into()
            }
        );
        let refused = json!({ "stop_reason": "refusal", "content": [] });
        assert!(parse_response(&refused).is_err());
        assert_eq!(cache_key("  How   MANY  days "), "how many days");
    }
}
