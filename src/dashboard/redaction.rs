use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

const REDACTED: &str = "***REDACTED***";

struct Patterns {
    pem: Regex,
    url_creds: Regex,
    bearer: Regex,
    known_tokens: Regex,
    url_query_secret: Regex,
    kv_quoted: Regex,
    kv_unquoted: Regex,
}

fn patterns() -> &'static Patterns {
    static LOCK: OnceLock<Patterns> = OnceLock::new();
    LOCK.get_or_init(|| Patterns {
        pem: Regex::new(
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        )
        .unwrap(),
        url_creds: Regex::new(r"(?i)([a-z][a-z0-9+\-.]*://)([^/\s:@]+):([^/\s@]+)@").unwrap(),
        bearer: Regex::new(r"(?i)\b(Bearer\s+)[A-Za-z0-9\-._~+/]+=*").unwrap(),
        known_tokens: Regex::new(
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{10,}\b|\bgithub_pat_[A-Za-z0-9_]{10,}\b|\bglpat-[A-Za-z0-9\-_]{10,}\b|\bsk-[A-Za-z0-9\-_]{12,}\b|\bxox[baprs]-[A-Za-z0-9\-]{10,}\b|\bAKIA[0-9A-Z]{16}\b",
        )
        .unwrap(),
        url_query_secret: Regex::new(
            r"(?i)([?&](?:token|access_token|api_key|apikey|secret|password|passwd|auth|key)=)([^&#\s]+)",
        )
        .unwrap(),
        kv_quoted: Regex::new(
            r#"(?i)\b(password|passwd|pwd|secret|token|api_key|apikey|access_key|secret_key|private_key|authorization)\b(\s*[:=]\s*)(?:"[^"\r\n]*"|'[^'\r\n]*')"#,
        )
        .unwrap(),
        kv_unquoted: Regex::new(
            r"(?i)\b(password|passwd|pwd|secret|token|api_key|apikey|access_key|secret_key|private_key|authorization)\b(\s*[:=]\s*)([^\s'\x22,;\]\)}]+)",
        )
        .unwrap(),
    })
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "token"
            | "access_token"
            | "refresh_token"
            | "csrf_token"
            | "secret"
            | "client_secret"
            | "password"
            | "passwd"
            | "pwd"
            | "api_key"
            | "apikey"
            | "access_key"
            | "secret_key"
            | "private_key"
            | "credential"
            | "credentials"
            | "authorization"
            | "cookie"
            | "cookies"
    ) || lower.ends_with("_token")
        || lower.ends_with("_secret")
        || lower.ends_with("_password")
        || lower.ends_with("_api_key")
}

pub fn redact_text(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let p = patterns();
    let s1 = p.pem.replace_all(input, REDACTED);
    let s2 = p.url_creds.replace_all(&s1, format!("${{1}}{REDACTED}@"));
    let s3 = p.bearer.replace_all(&s2, format!("${{1}}{REDACTED}"));
    let s4 = p.known_tokens.replace_all(&s3, REDACTED);
    let s5 = p
        .url_query_secret
        .replace_all(&s4, format!("${{1}}{REDACTED}"));
    let s6 = p
        .kv_quoted
        .replace_all(&s5, format!("${{1}}${{2}}\"{REDACTED}\""));
    let s7 = p.kv_unquoted.replace_all(&s6, format!("${{1}}${{2}}{REDACTED}"));
    s7.into_owned()
}

pub fn redact_value(val: &Value) -> Value {
    match val {
        Value::String(s) => Value::String(redact_text(s)),
        Value::Array(arr) => Value::Array(arr.iter().map(redact_value).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String(REDACTED.to_string()));
                } else {
                    out.insert(k.clone(), redact_value(v));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}
