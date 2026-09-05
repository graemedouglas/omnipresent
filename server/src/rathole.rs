use anyhow::{Context, Result};

use crate::config::Config;
use crate::db::Machine;

pub fn render_server_toml(cfg: &Config, machines: &[Machine]) -> String {
    let mut out = String::new();
    out.push_str("# Managed by omni — hand edits are overwritten. Run `omni sync` to regenerate.\n");
    out.push_str("[server]\n");
    out.push_str(&format!("bind_addr = \"{}\"\n", cfg.rathole.bind_addr));
    for m in machines.iter().filter(|m| !m.deleted) {
        out.push_str(&format!(
            "\n# {} ({})\n[server.services.\"{}\"]\ntoken = \"{}\"\nbind_addr = \"{}:{}\"\n",
            m.name, m.os.as_deref().unwrap_or("?"), m.id, m.service_token, m.bind_addr, m.port
        ));
    }
    out
}

/// Written in place (open + truncate), never via a symlink or rename:
/// rathole's config watcher ignores symlinks.
pub fn write_server_toml(cfg: &Config, machines: &[Machine]) -> Result<()> {
    let path = &cfg.rathole.config_path;
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(path, render_server_toml(cfg, machines))
        .with_context(|| format!("cannot write {path}"))
}

pub fn render_client_toml(cfg: &Config, m: &Machine) -> String {
    format!(
        "# Managed by omni — refresh with `omni update`.\n\
         [client]\n\
         remote_addr = \"{}\"\n\
         \n\
         [client.services.\"{}\"]\n\
         token = \"{}\"\n\
         local_addr = \"127.0.0.1:22\"\n",
        cfg.rathole.remote_addr, m.id, m.service_token
    )
}

/// Port of the rathole control endpoint as reachable from this host.
pub fn control_port(cfg: &Config) -> u16 {
    cfg.rathole
        .bind_addr
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(2333)
}
