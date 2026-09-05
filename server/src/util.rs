use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn rand_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    to_hex(&buf)
}

pub fn sha256_hex(input: &str) -> String {
    to_hex(&Sha256::digest(input.as_bytes()))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Seconds since `ts` (an RFC 3339 string we wrote ourselves); None if unparseable.
pub fn age_secs(ts: &str) -> Option<i64> {
    let t = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some((chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds())
}

/// Display names come from hostnames and CLI args; keep them shell- and
/// TOML-safe since they end up quoted in state files and config.
pub fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        format!("machine-{}", rand_hex(2))
    } else {
        cleaned
    }
}
