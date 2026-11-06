//! Bounded, credential-redacted diagnostics for explicit connection probes.
use std::{collections::HashMap, process::ExitStatus, sync::LazyLock};

use regex::Regex;

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap());
static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)((?:[\w-]*(?:api[_-]?key|token|secret|password)|authorization|cookie)["']?\s*[:=]\s*)(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;}]+)"#).unwrap()
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(bearer\s+)\S+").unwrap());
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:sk-|ghp_|github_pat_|AIza)[A-Za-z0-9_-]+").unwrap());
static URL_AUTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(https?://)[^\s/@]+(?::[^\s/@]*)?@").unwrap());

fn redact(text: &str, env: &HashMap<String, String>) -> String {
    let mut text = ANSI.replace_all(text, "").into_owned();
    // Explicit credential values must never escape through a CLI's error.
    // Ordinary flags such as NO_COLOR=1 must not corrupt every digit in an error.
    // Longest first prevents one value masking only a prefix of another.
    let mut values: Vec<&str> = env
        .iter()
        .filter(|(name, _)| {
            let name = name.to_ascii_lowercase();
            ["key", "token", "secret", "password", "auth", "credential", "cookie"]
                .iter()
                .any(|marker| name.contains(marker))
        })
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
        .collect();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in values {
        text = text.replace(value, "[redacted]");
    }
    text = URL_AUTH.replace_all(&text, "${1}[redacted]@").into_owned();
    text = BEARER.replace_all(&text, "${1}[redacted]").into_owned();
    text = CREDENTIAL.replace_all(&text, "${1}[redacted]").into_owned();
    text = TOKEN.replace_all(&text, "[redacted]").into_owned();
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}

pub(super) fn format_probe_failure(
    error: &str,
    exit: Option<ExitStatus>,
    stderr: &str,
    env: &HashMap<String, String>,
) -> String {
    let mut message = redact(error, env);
    message.push_str("\nProcess: ");
    match exit {
        Some(status) => message.push_str(&status.to_string()),
        None => message.push_str("no exit status observed before probe cleanup"),
    }
    message.push_str("\nCLI stderr:\n");
    let stderr = redact(stderr, env);
    if stderr.trim().is_empty() {
        message.push_str("No stderr output captured. The underlying cause is not available.");
    } else {
        message.push_str(stderr.trim());
    }
    const LIMIT: usize = 8_000;
    if message.chars().count() > LIMIT {
        message = message.chars().take(LIMIT).collect();
        message.push_str("\n[diagnostic truncated]");
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_diagnostics_redact_credentials_and_preserve_module_errors() {
        let env = HashMap::from([("CUSTOM_AUTH".into(), "private-value".into())]);
        let message = format_probe_failure(
            "ACP initialize failed",
            None,
            "\x1b[31mError: Cannot find module '/cache/_npx/abc/module.js'\x1b[0m\nOPENAI_API_KEY=sk-abc123\n{\"access_token\":\"jwt-secret\"}\nAuthorization: Bearer hidden\nhttps://user:pass@example.com\nprivate-value",
            &env,
        );
        for secret in [
            "sk-abc123",
            "jwt-secret",
            "hidden",
            "user:pass",
            "private-value",
            "\x1b",
        ] {
            assert!(!message.contains(secret), "leaked {secret}: {message}");
        }
        assert!(message.contains("Cannot find module '/cache/_npx/abc/module.js'"));
        assert!(message.contains("no exit status observed"));
    }

    #[test]
    fn probe_diagnostics_empty_and_large_unicode_output() {
        let message = format_probe_failure("ACP session/new timed out", None, "", &HashMap::new());
        assert!(message.contains("No stderr output captured"));
        let message = format_probe_failure("ACP initialize failed", None, &"错误".repeat(9_000), &HashMap::new());
        assert!(message.ends_with("[diagnostic truncated]"));
        assert!(message.chars().count() < 8_100);
    }
}
