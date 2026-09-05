use std::sync::Mutex;

use anyhow::{bail, Result};
use rusqlite::Connection;

use crate::config::Config;
use crate::db::{self, Machine};
use crate::rathole;
use crate::termix::Termix;
use crate::util;

pub struct App {
    pub cfg: Config,
    pub db: Mutex<Connection>,
    pub termix: Option<Termix>,
}

impl App {
    pub fn new(cfg: Config) -> Result<App> {
        let conn = db::open(&cfg.db_path)?;
        let termix = cfg.termix.as_ref().map(Termix::new);
        Ok(App {
            cfg,
            db: Mutex::new(conn),
            termix,
        })
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().expect("db mutex poisoned")
    }
}

fn write_rathole(app: &App) -> Result<()> {
    let machines = db::live_machines(&app.lock())?;
    rathole::write_server_toml(&app.cfg, &machines)
}

/// Name shown in Termix; stale machines get flagged so a dead tunnel doesn't
/// look like a broken connection.
fn termix_display_name(m: &Machine) -> String {
    if m.marked_stale {
        format!("{} (down)", m.name)
    } else {
        m.name.clone()
    }
}

/// Upsert the machine's Termix host and persist the host id. Termix being
/// down never fails the caller's main job; `omni sync` reconciles later.
pub async fn sync_termix_host(app: &App, machine_id: &str) -> Result<()> {
    let Some(termix) = &app.termix else {
        return Ok(());
    };
    let Some(mut m) = db::machine_by_id(&app.lock(), machine_id)? else {
        return Ok(());
    };
    let host_id = termix
        .upsert_host(
            m.termix_host_id.as_deref(),
            &termix_display_name(&m),
            "127.0.0.1",
            m.port,
        )
        .await?;
    if m.termix_host_id.as_deref() != Some(host_id.as_str()) {
        m.termix_host_id = Some(host_id);
        db::update_machine(&app.lock(), &m)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ enroll ---

pub struct Enrolled {
    pub machine: Machine,
    pub secret: String,
    pub client_toml: String,
}

pub async fn enroll(
    app: &App,
    token: &str,
    name: Option<&str>,
    os: Option<&str>,
) -> Result<Enrolled> {
    let secret = util::rand_hex(32);
    let machine = {
        let conn = app.lock();
        let hint = db::consume_token(&conn, token)?;
        let name = util::sanitize_name(
            name.filter(|s| !s.trim().is_empty())
                .or(hint.as_deref())
                .unwrap_or("machine"),
        );
        // Re-enrolling an existing name updates it — never a duplicate.
        // Both secrets rotate: the machine gets a fresh client.toml anyway.
        match db::machine_by_name(&conn, &name)? {
            Some(mut m) => {
                m.service_token = util::rand_hex(32);
                m.secret_hash = util::sha256_hex(&secret);
                m.os = os.map(str::to_string).or(m.os);
                m.last_seen = Some(util::now_rfc3339());
                m.marked_stale = false;
                db::update_machine(&conn, &m)?;
                m
            }
            None => {
                let m = Machine {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    port: db::next_port(&conn, app.cfg.port_start)?,
                    service_token: util::rand_hex(32),
                    secret_hash: util::sha256_hex(&secret),
                    bind_addr: "127.0.0.1".to_string(),
                    termix_host_id: None,
                    os: os.map(str::to_string),
                    enrolled_at: util::now_rfc3339(),
                    last_seen: Some(util::now_rfc3339()),
                    deleted: false,
                    marked_stale: false,
                };
                db::insert_machine(&conn, &m)?;
                m
            }
        }
    };
    write_rathole(app)?;
    if let Err(e) = sync_termix_host(app, &machine.id).await {
        eprintln!("warning: enrolled {} but Termix update failed: {e:#}", machine.name);
    }
    let client_toml = rathole::render_client_toml(&app.cfg, &machine);
    Ok(Enrolled {
        machine,
        secret,
        client_toml,
    })
}

// --------------------------------------------------------------- CLI verbs ---

pub fn token_new(app: &App, name: Option<&str>) -> Result<()> {
    let token = util::rand_hex(16);
    db::insert_token(
        &app.lock(),
        &util::sha256_hex(&token),
        name,
        15 * 60,
    )?;
    println!("token (single-use, expires in 15 min):\n");
    println!(
        "  curl -fsSL {}/install.sh | sudo sh -s -- {token}",
        app.cfg.public_url_trimmed()
    );
    Ok(())
}

async fn port_connected(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_millis(400),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Ok(_))
    )
}

fn human_age(ts: Option<&str>) -> String {
    let Some(secs) = ts.and_then(util::age_secs) else {
        return "never".to_string();
    };
    match secs {
        s if s < 0 => "future?".into(),
        s if s < 90 => format!("{s}s ago"),
        s if s < 5400 => format!("{}m ago", s / 60),
        s if s < 172800 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

pub async fn ls(app: &App) -> Result<()> {
    let machines = db::live_machines(&app.lock())?;
    if machines.is_empty() {
        println!("no machines enrolled — mint a token with `omni token new`");
        return Ok(());
    }
    println!(
        "{:<20} {:>5}  {:<7} {:<10} {:<12} ID",
        "NAME", "PORT", "OS", "CONNECTED", "LAST SEEN"
    );
    for m in machines {
        let connected = if port_connected(m.port).await { "yes" } else { "no" };
        let exposure = if m.bind_addr == "127.0.0.1" { "" } else { "  [exposed]" };
        println!(
            "{:<20} {:>5}  {:<7} {:<10} {:<12} {}{}",
            m.name,
            m.port,
            m.os.as_deref().unwrap_or("?"),
            connected,
            human_age(m.last_seen.as_deref()),
            &m.id[..8],
            exposure,
        );
    }
    Ok(())
}

pub async fn show(app: &App, needle: &str) -> Result<()> {
    let m = db::find_machine(&app.lock(), needle)?;
    let connected = if port_connected(m.port).await { "yes" } else { "no" };
    println!("name:        {}", m.name);
    println!("id:          {}", m.id);
    println!("port:        {} (bind {})", m.port, m.bind_addr);
    println!("os:          {}", m.os.as_deref().unwrap_or("?"));
    println!("connected:   {connected}");
    println!("enrolled:    {}", m.enrolled_at);
    println!("last seen:   {}", human_age(m.last_seen.as_deref()));
    println!("termix host: {}", m.termix_host_id.as_deref().unwrap_or("-"));
    println!("stale flag:  {}", if m.marked_stale { "down" } else { "ok" });
    Ok(())
}

pub async fn rename(app: &App, needle: &str, new_name: &str) -> Result<()> {
    let new_name = util::sanitize_name(new_name);
    let m = {
        let conn = app.lock();
        let mut m = db::find_machine(&conn, needle)?;
        if let Some(other) = db::machine_by_name(&conn, &new_name)? {
            if other.id != m.id {
                bail!("name '{new_name}' is taken by machine {}", &other.id[..8]);
            }
        }
        m.name = new_name.clone();
        db::update_machine(&conn, &m)?;
        m
    };
    write_rathole(app)?;
    sync_termix_host(app, &m.id).await?;
    println!("renamed to {new_name} (Termix host updated)");
    Ok(())
}

pub async fn rm(app: &App, needle: &str) -> Result<()> {
    let m = db::find_machine(&app.lock(), needle)?;
    db::mark_deleted(&app.lock(), &m.id)?;
    write_rathole(app)?;
    if let Some(termix) = &app.termix {
        if let Some(host_id) = &m.termix_host_id {
            if let Err(e) = termix.delete_host(host_id).await {
                eprintln!("warning: Termix host not deleted: {e:#}");
            }
        }
    }
    println!(
        "revoked {} — stanza dropped from server.toml, tunnel dies on next reconnect; port {} is retired",
        m.name, m.port
    );
    Ok(())
}

pub async fn set_exposure(app: &App, needle: &str, exposed: bool) -> Result<()> {
    let bind = if exposed { "0.0.0.0" } else { "127.0.0.1" };
    let m = {
        let conn = app.lock();
        let mut m = db::find_machine(&conn, needle)?;
        m.bind_addr = bind.to_string();
        db::update_machine(&conn, &m)?;
        m
    };
    write_rathole(app)?;
    if exposed {
        println!(
            "{} now binds 0.0.0.0:{} — direct SSH from anywhere; open the port in the firewall yourself",
            m.name, m.port
        );
    } else {
        println!("{} back on loopback — only Termix reaches port {}", m.name, m.port);
    }
    Ok(())
}

pub async fn sync(app: &App) -> Result<()> {
    write_rathole(app)?;
    println!("server.toml regenerated at {}", app.cfg.rathole.config_path);
    let machines = db::live_machines(&app.lock())?;
    if app.termix.is_none() {
        println!("termix: not configured, skipped");
        return Ok(());
    }
    for m in &machines {
        match sync_termix_host(app, &m.id).await {
            Ok(()) => println!("termix: {} ok", m.name),
            Err(e) => println!("termix: {} FAILED: {e:#}", m.name),
        }
    }
    Ok(())
}

pub async fn doctor(app: &App) -> Result<()> {
    let mut failed = false;
    let mut check = |name: &str, res: Result<String>| match res {
        Ok(detail) => println!("  ok   {name}: {detail}"),
        Err(e) => {
            failed = true;
            println!("  FAIL {name}: {e:#}");
        }
    };

    check("database", {
        db::live_machines(&app.lock()).map(|m| format!("{} ({} machines)", app.cfg.db_path, m.len()))
    });

    check("rathole config", {
        let path = &app.cfg.rathole.config_path;
        let machines = db::live_machines(&app.lock())?;
        rathole::write_server_toml(&app.cfg, &machines)
            .map(|()| format!("{path} writable, regenerated"))
    });

    let port = rathole::control_port(&app.cfg);
    check(
        "rathole control",
        if port_connected(port).await {
            Ok(format!("listening on :{port}"))
        } else {
            Err(anyhow::anyhow!(
                "nothing listening on 127.0.0.1:{port} — is the rathole server running?"
            ))
        },
    );

    match &app.termix {
        Some(termix) => check(
            "termix",
            termix
                .ping()
                .await
                .map(|()| app.cfg.termix.as_ref().unwrap().url.clone()),
        ),
        None => println!("  --   termix: not configured (machines won't appear in the browser)"),
    }

    check(
        "public_url",
        if app.cfg.public_url.starts_with("http") {
            Ok(app.cfg.public_url_trimmed().to_string())
        } else {
            Err(anyhow::anyhow!("'{}' is not a URL", app.cfg.public_url))
        },
    );

    if failed {
        bail!("doctor found problems");
    }
    println!("all good");
    Ok(())
}

// ----------------------------------------------------------------- staleness --

/// Flip the Termix "(down)" marker as heartbeats stop and resume.
pub async fn stale_sweep(app: &App) -> Result<()> {
    let Some(termix_cfg) = &app.cfg.termix else {
        return Ok(());
    };
    let threshold = termix_cfg.stale_after_secs;
    let machines = db::live_machines(&app.lock())?;
    for m in machines {
        let age = m
            .last_seen
            .as_deref()
            .or(Some(m.enrolled_at.as_str()))
            .and_then(util::age_secs)
            .unwrap_or(0);
        let stale = age > threshold;
        if stale == m.marked_stale {
            continue;
        }
        {
            let conn = app.lock();
            let mut fresh = match db::machine_by_id(&conn, &m.id)? {
                Some(f) => f,
                None => continue,
            };
            fresh.marked_stale = stale;
            db::update_machine(&conn, &fresh)?;
        }
        if let Err(e) = sync_termix_host(app, &m.id).await {
            eprintln!("warning: stale marker for {} not pushed to Termix: {e:#}", m.name);
        } else {
            println!(
                "{} marked {} in Termix",
                m.name,
                if stale { "down" } else { "up" }
            );
        }
    }
    Ok(())
}
