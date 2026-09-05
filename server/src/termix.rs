//! Termix API client.
//!
//! NOTE: the endpoint paths and payload shape below are the one piece of omni
//! written against an API we don't control and couldn't verify offline. Check
//! them against your deployed Termix version (its OpenAPI/docs or the network
//! tab while adding a host in the UI) and adjust in this one file if needed.
//! Everything else in omni is insulated from Termix behind these functions.

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
            .req(reqwest::Method::GET, "/ssh/db/host")
            .send()
            .await
            .context("Termix unreachable")?;
        if !resp.status().is_success() {
            bail!("Termix answered {} — check termix.url and api_key", resp.status());
        }
        Ok(())
    }

    fn host_payload(name: &str, ip: &str, port: u16) -> serde_json::Value {
        json!({
            "name": name,
            "ip": ip,
            "port": port,
        })
    }

    /// Create or update; returns the Termix host id for idempotent updates.
    pub async fn upsert_host(
        &self,
        existing_id: Option<&str>,
        name: &str,
        ip: &str,
        port: u16,
    ) -> Result<String> {
        let payload = Self::host_payload(name, ip, port);
        if let Some(id) = existing_id {
            let r = self
                .req(reqwest::Method::PUT, &format!("/ssh/db/host/{id}"))
                .json(&payload)
                .send()
                .await
                .context("Termix unreachable")?;
            if r.status().is_success() {
                return Ok(id.to_string());
            }
            // Host deleted in the UI: fall through and recreate it.
            if r.status() != reqwest::StatusCode::NOT_FOUND {
                bail!("Termix host update failed: {}", r.status());
            }
        }
        let r = self
            .req(reqwest::Method::POST, "/ssh/db/host")
            .json(&payload)
            .send()
            .await
            .context("Termix unreachable")?;
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Termix host create failed: {status} {}", truncate(&body));
        }
        let v: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("Termix create response is not JSON: {}", truncate(&body)))?;
        let id = v
            .get("id")
            .or_else(|| v.get("hostId"))
            .or_else(|| v.get("data").and_then(|d| d.get("id")));
        match id {
            Some(serde_json::Value::String(s)) => Ok(s.clone()),
            Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
            _ => bail!("Termix create response has no host id: {}", truncate(&body)),
        }
    }

    pub async fn delete_host(&self, id: &str) -> Result<()> {
        let r = self
            .req(reqwest::Method::DELETE, &format!("/ssh/db/host/{id}"))
            .send()
            .await
            .context("Termix unreachable")?;
        if !r.status().is_success() && r.status() != reqwest::StatusCode::NOT_FOUND {
            bail!("Termix host delete failed: {}", r.status());
        }
        Ok(())
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
