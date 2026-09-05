use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use crate::db;
use crate::ops::{self, App};
use crate::rathole;

const INSTALL_SH: &str = include_str!("../../client/install.sh");

pub async fn serve(app: Arc<App>) -> Result<()> {
    startup_checks(&app).await?;

    let router = Router::new()
        .route("/enroll", post(enroll))
        .route("/config", get(get_config))
        .route("/heartbeat", post(heartbeat))
        .route("/install.sh", get(install_sh))
        .route("/install.ps1", get(install_ps1))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(app.clone());

    tokio::spawn(stale_loop(app.clone()));

    let listener = tokio::net::TcpListener::bind(&app.cfg.listen)
        .await
        .with_context(|| format!("cannot bind {}", app.cfg.listen))?;
    println!("omni listening on {} (public: {})", app.cfg.listen, app.cfg.public_url_trimmed());
    axum::serve(listener, router).await?;
    Ok(())
}

/// omni assumes rathole and Termix are already on this box (Phase 0's output);
/// refuse to run with a clear error rather than limp along.
async fn startup_checks(app: &App) -> Result<()> {
    let machines = db::live_machines(&app.lock())?;
    rathole::write_server_toml(&app.cfg, &machines).context(
        "rathole config not writable — is rathole set up? (omni doctor)",
    )?;
    if let Some(termix) = &app.termix {
        termix
            .ping()
            .await
            .context("Termix not reachable — fix termix.url/api_key or remove [termix] from the config")?;
    } else {
        eprintln!("warning: [termix] not configured — machines will tunnel but won't appear in the browser");
    }
    Ok(())
}

async fn stale_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tick.tick().await;
        if let Err(e) = ops::stale_sweep(&app).await {
            eprintln!("warning: stale sweep failed: {e:#}");
        }
    }
}

// ---------------------------------------------------------------- handlers ---

type HttpErr = (StatusCode, String);

fn internal(e: anyhow::Error) -> HttpErr {
    eprintln!("error: {e:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n".into())
}

#[derive(Deserialize)]
struct EnrollForm {
    token: String,
    name: Option<String>,
    os: Option<String>,
}

async fn enroll(
    State(app): State<Arc<App>>,
    Form(form): Form<EnrollForm>,
) -> Result<Response, HttpErr> {
    let enrolled = ops::enroll(&app, &form.token, form.name.as_deref(), form.os.as_deref())
        .await
        .map_err(|e| {
            // Token problems are the caller's fault and safe to state plainly.
            let msg = format!("{e:#}");
            if msg.contains("token") {
                (StatusCode::FORBIDDEN, format!("{msg}\n"))
            } else {
                internal(e)
            }
        })?;
    let body = format!(
        "OMNI-ENROLL-V1\nmachine_id={}\nmachine_secret={}\nname={}\nport={}\nrathole_version={}\n---BEGIN CLIENT TOML---\n{}---END CLIENT TOML---\n",
        enrolled.machine.id,
        enrolled.secret,
        enrolled.machine.name,
        enrolled.machine.port,
        app.cfg.rathole.version,
        enrolled.client_toml,
    );
    println!(
        "enrolled {} (port {}, {})",
        enrolled.machine.name,
        enrolled.machine.port,
        enrolled.machine.os.as_deref().unwrap_or("?")
    );
    Ok(([("content-type", "text/plain")], body).into_response())
}

fn machine_auth(app: &App, headers: &HeaderMap) -> Result<db::Machine, HttpErr> {
    let get = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let (Some(id), Some(secret)) = (get("x-omni-machine"), get("x-omni-secret")) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing X-Omni-Machine / X-Omni-Secret headers\n".into(),
        ));
    };
    db::auth_machine(&app.lock(), &id, &secret)
        .map_err(|_| (StatusCode::FORBIDDEN, "bad machine credentials\n".into()))
}

async fn get_config(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Response, HttpErr> {
    let m = machine_auth(&app, &headers)?;
    db::touch_last_seen(&app.lock(), &m.id).map_err(internal)?;
    let toml = rathole::render_client_toml(&app.cfg, &m);
    Ok((
        [
            ("content-type", "text/plain".to_string()),
            ("x-omni-rathole-version", app.cfg.rathole.version.clone()),
            ("x-omni-port", m.port.to_string()),
            ("x-omni-name", m.name.clone()),
        ],
        toml,
    )
        .into_response())
}

async fn heartbeat(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Response, HttpErr> {
    let m = machine_auth(&app, &headers)?;
    db::touch_last_seen(&app.lock(), &m.id).map_err(internal)?;
    Ok("ok\n".into_response())
}

async fn install_sh(State(app): State<Arc<App>>) -> Response {
    let body = INSTALL_SH
        .replace("__OMNI_URL__", app.cfg.public_url_trimmed())
        .replace("__RATHOLE_VERSION__", &app.cfg.rathole.version)
        .replace("__HERDR_INSTALL_URL__", &app.cfg.herdr.install_url);
    ([("content-type", "text/x-shellscript")], body).into_response()
}

async fn install_ps1() -> Response {
    (
        StatusCode::NOT_FOUND,
        "# The PowerShell client isn't built yet (build plan phase 4).\n",
    )
        .into_response()
}
