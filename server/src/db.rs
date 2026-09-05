use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, Row};

use crate::util;

#[derive(Debug, Clone)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub service_token: String,
    pub secret_hash: String,
    pub bind_addr: String,
    pub termix_host_id: Option<String>,
    pub os: Option<String>,
    pub enrolled_at: String,
    pub last_seen: Option<String>,
    pub deleted: bool,
    pub marked_stale: bool,
}

pub fn open(path: &str) -> Result<Connection> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("cannot open db {path}"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS machines (
            id              TEXT PRIMARY KEY,
            name            TEXT NOT NULL,
            port            INTEGER NOT NULL UNIQUE,
            service_token   TEXT NOT NULL,
            secret_hash     TEXT NOT NULL,
            bind_addr       TEXT NOT NULL DEFAULT '127.0.0.1',
            termix_host_id  TEXT,
            os              TEXT,
            enrolled_at     TEXT NOT NULL,
            last_seen       TEXT,
            deleted         INTEGER NOT NULL DEFAULT 0,
            marked_stale    INTEGER NOT NULL DEFAULT 0
        );
        CREATE UNIQUE INDEX IF NOT EXISTS machines_live_name
            ON machines(name) WHERE deleted = 0;
        CREATE TABLE IF NOT EXISTS tokens (
            id          TEXT PRIMARY KEY,
            token_hash  TEXT NOT NULL,
            name_hint   TEXT,
            created_at  TEXT NOT NULL,
            expires_at  TEXT NOT NULL,
            used_at     TEXT
        );
        "#,
    )?;
    Ok(conn)
}

fn row_to_machine(row: &Row) -> rusqlite::Result<Machine> {
    Ok(Machine {
        id: row.get("id")?,
        name: row.get("name")?,
        port: row.get::<_, i64>("port")? as u16,
        service_token: row.get("service_token")?,
        secret_hash: row.get("secret_hash")?,
        bind_addr: row.get("bind_addr")?,
        termix_host_id: row.get("termix_host_id")?,
        os: row.get("os")?,
        enrolled_at: row.get("enrolled_at")?,
        last_seen: row.get("last_seen")?,
        deleted: row.get::<_, i64>("deleted")? != 0,
        marked_stale: row.get::<_, i64>("marked_stale")? != 0,
    })
}

const COLS: &str = "id, name, port, service_token, secret_hash, bind_addr, \
                    termix_host_id, os, enrolled_at, last_seen, deleted, marked_stale";

pub fn live_machines(conn: &Connection) -> Result<Vec<Machine>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM machines WHERE deleted = 0 ORDER BY port"
    ))?;
    let rows = stmt.query_map([], row_to_machine)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn machine_by_id(conn: &Connection, id: &str) -> Result<Option<Machine>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM machines WHERE id = ?1 AND deleted = 0"
    ))?;
    let mut rows = stmt.query_map(params![id], row_to_machine)?;
    Ok(rows.next().transpose()?)
}

pub fn machine_by_name(conn: &Connection, name: &str) -> Result<Option<Machine>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM machines WHERE name = ?1 AND deleted = 0"
    ))?;
    let mut rows = stmt.query_map(params![name], row_to_machine)?;
    Ok(rows.next().transpose()?)
}

/// CLI lookup: exact name, exact id, then unique id prefix.
pub fn find_machine(conn: &Connection, needle: &str) -> Result<Machine> {
    if let Some(m) = machine_by_name(conn, needle)? {
        return Ok(m);
    }
    if let Some(m) = machine_by_id(conn, needle)? {
        return Ok(m);
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM machines WHERE id LIKE ?1 || '%' AND deleted = 0"
    ))?;
    let rows = stmt.query_map(params![needle], row_to_machine)?;
    let matches = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    match matches.len() {
        0 => bail!("no machine matches '{needle}' (try `omni ls`)"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => bail!("'{needle}' is ambiguous ({n} machines match)"),
    }
}

/// Ports are pinned to ids and never reused: deleted rows keep their port,
/// and allocation only ever counts up.
pub fn next_port(conn: &Connection, port_start: u16) -> Result<u16> {
    let port: i64 = conn.query_row(
        "SELECT COALESCE(MAX(port) + 1, ?1) FROM machines",
        params![port_start as i64],
        |r| r.get(0),
    )?;
    if port > u16::MAX as i64 {
        bail!("port space exhausted");
    }
    Ok(port as u16)
}

pub fn insert_machine(conn: &Connection, m: &Machine) -> Result<()> {
    conn.execute(
        "INSERT INTO machines (id, name, port, service_token, secret_hash, bind_addr, \
                               termix_host_id, os, enrolled_at, last_seen, deleted, marked_stale) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 0)",
        params![
            m.id,
            m.name,
            m.port as i64,
            m.service_token,
            m.secret_hash,
            m.bind_addr,
            m.termix_host_id,
            m.os,
            m.enrolled_at,
            m.last_seen,
        ],
    )?;
    Ok(())
}

pub fn update_machine(conn: &Connection, m: &Machine) -> Result<()> {
    let n = conn.execute(
        "UPDATE machines SET name = ?2, service_token = ?3, secret_hash = ?4, bind_addr = ?5, \
                             termix_host_id = ?6, os = ?7, last_seen = ?8, marked_stale = ?9 \
         WHERE id = ?1",
        params![
            m.id,
            m.name,
            m.service_token,
            m.secret_hash,
            m.bind_addr,
            m.termix_host_id,
            m.os,
            m.last_seen,
            m.marked_stale as i64,
        ],
    )?;
    if n != 1 {
        bail!("machine {} not found for update", m.id);
    }
    Ok(())
}

pub fn touch_last_seen(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE machines SET last_seen = ?2 WHERE id = ?1",
        params![id, util::now_rfc3339()],
    )?;
    Ok(())
}

pub fn mark_deleted(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE machines SET deleted = 1 WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn auth_machine(conn: &Connection, id: &str, secret: &str) -> Result<Machine> {
    let m = machine_by_id(conn, id)?.ok_or_else(|| anyhow!("unknown machine"))?;
    if m.secret_hash != util::sha256_hex(secret) {
        bail!("bad machine secret");
    }
    Ok(m)
}

// ------------------------------------------------------------------ tokens ---

pub fn insert_token(
    conn: &Connection,
    token_hash: &str,
    name_hint: Option<&str>,
    ttl_secs: i64,
) -> Result<()> {
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::seconds(ttl_secs);
    conn.execute(
        "INSERT INTO tokens (id, token_hash, name_hint, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            token_hash,
            name_hint,
            now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ],
    )?;
    Ok(())
}

/// Single use: marks the token consumed on success. Returns the name hint.
pub fn consume_token(conn: &Connection, token: &str) -> Result<Option<String>> {
    let now = util::now_rfc3339();
    conn.execute("DELETE FROM tokens WHERE expires_at < ?1 AND used_at IS NULL", params![now])?;
    let hash = util::sha256_hex(token);
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT id, name_hint FROM tokens \
             WHERE token_hash = ?1 AND used_at IS NULL AND expires_at >= ?2",
            params![hash, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let Some((id, hint)) = row else {
        bail!("enrollment token is invalid, expired, or already used");
    };
    conn.execute(
        "UPDATE tokens SET used_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(hint)
}
