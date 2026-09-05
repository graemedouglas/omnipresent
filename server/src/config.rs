use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "d_db_path")]
    pub db_path: String,
    #[serde(default = "d_listen")]
    pub listen: String,
    /// Where clients reach omni, e.g. "https://omni.example.tld". Baked into
    /// the served install.sh and the printed enrollment one-liners.
    pub public_url: String,
    #[serde(default = "d_port_start")]
    pub port_start: u16,
    pub rathole: RatholeCfg,
    #[serde(default)]
    pub termix: Option<TermixCfg>,
    #[serde(default)]
    pub herdr: HerdrCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatholeCfg {
    /// The server.toml omni owns. Written in place — rathole's config watcher
    /// ignores symlinks, so this must be the real file.
    #[serde(default = "d_rathole_config")]
    pub config_path: String,
    /// What the rathole server binds, e.g. "0.0.0.0:2333".
    #[serde(default = "d_rathole_bind")]
    pub bind_addr: String,
    /// What enrolled clients dial, e.g. "omni.example.tld:2333".
    pub remote_addr: String,
    /// Version pin; re-running enroll or `omni update` upgrades clients to it.
    #[serde(default = "d_rathole_version")]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermixCfg {
    /// Termix base URL as reached from the VPS, e.g. "http://127.0.0.1:8090".
    pub url: String,
    pub api_key: String,
    /// Seconds without a heartbeat before a machine is marked down in Termix.
    #[serde(default = "d_stale_after")]
    pub stale_after_secs: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrCfg {
    /// Piped to sh by `install.sh --with-herdr`; set "" to disable the offer.
    #[serde(default = "d_herdr_url")]
    pub install_url: String,
}

impl Default for HerdrCfg {
    fn default() -> Self {
        HerdrCfg {
            install_url: d_herdr_url(),
        }
    }
}

fn d_herdr_url() -> String {
    // Verified official installer (herdrdev/herdr README, Sept 2026).
    "https://herdr.dev/install.sh".into()
}

fn d_db_path() -> String {
    "/var/lib/omni/omni.db".into()
}
fn d_listen() -> String {
    "127.0.0.1:8070".into()
}
fn d_port_start() -> u16 {
    2201
}
fn d_rathole_config() -> String {
    "/etc/rathole/server.toml".into()
}
fn d_rathole_bind() -> String {
    "0.0.0.0:2333".into()
}
fn d_rathole_version() -> String {
    "0.5.0".into()
}
fn d_stale_after() -> i64 {
    900
}

impl Config {
    pub fn load(path: &str) -> Result<Config> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {path} (see omni.example.toml)"))?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("cannot parse config {path}"))?;
        Ok(cfg)
    }

    pub fn public_url_trimmed(&self) -> &str {
        self.public_url.trim_end_matches('/')
    }
}
