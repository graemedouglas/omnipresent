//! Termix API client.
//!
//! Verified against Termix-SSH/Termix main (September 2026),
//! src/backend/database/routes/host.ts and docker/nginx.conf:
//!
//! - All paths go to the same port Termix serves its UI on; the bundled
//!   nginx proxies `/host/` to the backend.
//! - Auth is `Authorization: Bearer tmx_...` — a user-scoped API key minted
//!   in the Termix UI (API keys start with `tmx_`).
//! - `POST /host/enroll` creates a host and is the intended machine path: it
//!   *requires* API-key auth (401 `API_KEY_REQUIRED` on a plain session) and
//!   applies SSH defaults (connectionType=ssh, authType=none, enableTerminal,
//!   enableSsh). Validation needs a non-empty `ip` and a valid `port`.
//!   Returns the host object as JSON, including its numeric `id`.
//! - `GET/PUT/DELETE /host/db/host/{id}` read, replace, and delete; all 404
//!   when the host is gone (e.g. deleted in the UI).
//! - A 423 `DATA_LOCKED` means the API key owner's data-encryption key isn't
//!   unlocked server-side; they need to sign in to Termix once.
//!
//! PUT is a full replace, so updates here are read-modify-write: fetch the
//! host, override only what omni owns (name, ip, port), send it back. That
//! preserves toggles someone flipped in the UI. Sensitive fields (passwords,
//! keys) are stripped from GET responses, so inline credentials added in the
//! UI may not survive an update — omni-owned hosts default to authType=none,
//! where that doesn't matter; credential *references* (credentialId) survive.

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::TermixCfg;

pub struct Termix {
    base: String,
    api_key: String,
    http: reqwest::Client,
}

impl Termix {
    pub fn new(cfg: &TermixCfg) -> Termix {
        Termix {
            base: cfg.url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", self.base, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
    }

    pub async fn ping(&self) -> Result<()> {
        let resp = self
            .req(reqwest::Method::GET, "/host/db/host")
            .send()
            .await
            .context("Termix unreachable")?;
        if !resp.status().is_success() {
            bail!("{}", explain_status(resp.status()));
        }
        Ok(())
    }

    /// Create or update; returns the Termix host id for idempotent updates.
    pub async fn upsert_host(
        &self,
        existing_id: Option<&str>,
        name: &str,
        ip: &str,
        port: u16,
    ) -> Result<String> {
        if let Some(id) = existing_id {
            if self.update_host(id, name, ip, port).await? {
                return Ok(id.to_string());
            }
            // host deleted in the UI — recreate below
        }
        self.create_host(name, ip, port).await
    }

    async fn create_host(&self, name: &str, ip: &str, port: u16) -> Result<String> {
        let payload = json!({
            "name": name,
            "ip": ip,
            "port": port,
        });
        let r = self
            .req(reqwest::Method::POST, "/host/enroll")
            .json(&payload)
            .send()
            .await
            .context("Termix unreachable")?;
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Termix host create failed: {} {}", explain_status(status), truncate(&body));
        }
        let v: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("Termix create response is not JSON: {}", truncate(&body)))?;
        match v.get("id") {
            Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
            Some(serde_json::Value::String(s)) => Ok(s.clone()),
            _ => bail!("Termix create response has no host id: {}", truncate(&body)),
        }
    }

    /// Ok(false) means the host no longer exists and should be recreated.
    async fn update_host(&self, id: &str, name: &str, ip: &str, port: u16) -> Result<bool> {
        let r = self
            .req(reqwest::Method::GET, &format!("/host/db/host/{id}"))
            .send()
            .await
            .context("Termix unreachable")?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !r.status().is_success() {
            bail!("Termix host fetch failed: {}", explain_status(r.status()));
        }
        let mut host: serde_json::Value =
            r.json().await.context("Termix host response is not JSON")?;
        let Some(obj) = host.as_object_mut() else {
            bail!("Termix host response is not an object");
        };
        obj.insert("name".into(), json!(name));
        obj.insert("ip".into(), json!(ip));
        obj.insert("port".into(), json!(port));

        let r = self
            .req(reqwest::Method::PUT, &format!("/host/db/host/{id}"))
            .json(&host)
            .send()
            .await
            .context("Termix unreachable")?;
        match r.status() {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => bail!("Termix host update failed: {}", explain_status(s)),
        }
    }

    pub async fn delete_host(&self, id: &str) -> Result<()> {
        let r = self
            .req(reqwest::Method::DELETE, &format!("/host/db/host/{id}"))
            .send()
            .await
            .context("Termix unreachable")?;
        if !r.status().is_success() && r.status() != reqwest::StatusCode::NOT_FOUND {
            bail!("Termix host delete failed: {}", explain_status(r.status()));
        }
        Ok(())
    }
}

fn explain_status(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 => "401 — Termix rejected the API key (needs a tmx_… key from the Termix UI)".into(),
        423 => "423 — Termix user data is locked; sign in to Termix once as the key's owner".into(),
        s => format!("{s}"),
    }
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 200 {
        format!("{}…", s.chars().take(200).collect::<String>())
    } else {
        s.to_string()
    }
}
