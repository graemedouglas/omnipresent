mod config;
mod db;
mod ops;
mod rathole;
mod serve;
mod termix;
mod util;

use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "omni",
    version,
    about = "Fleet enrollment: one command on a new machine, thirty seconds later it's in Termix"
)]
struct Cli {
    /// Path to omni's own config
    #[arg(long, global = true, env = "OMNI_CONFIG", default_value = "/etc/omni/omni.toml")]
    config: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the HTTP service
    Serve,
    /// Enrollment tokens
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Machines: name, port, os, connected, last seen
    Ls,
    /// Detail on one machine
    Show { machine: String },
    /// Rename a machine (updates the Termix host too)
    Rename { machine: String, name: String },
    /// Revoke: drop the rathole stanza, delete the Termix host
    Rm { machine: String },
    /// Bind the machine's port on 0.0.0.0 — direct SSH, phone clients
    Expose { machine: String },
    /// Back to loopback
    Unexpose { machine: String },
    /// Reconcile server.toml + Termix against the DB
    Sync,
    /// rathole reachable? Termix API? config writable?
    Doctor,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Mint an enrollment token and print the one-liner
    New {
        /// Pre-set the machine's display name
        #[arg(long)]
        name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load(&cli.config)?;
    let app = Arc::new(ops::App::new(cfg)?);
    match cli.cmd {
        Cmd::Serve => serve::serve(app).await,
        Cmd::Token { cmd: TokenCmd::New { name } } => ops::token_new(&app, name.as_deref()),
        Cmd::Ls => ops::ls(&app).await,
        Cmd::Show { machine } => ops::show(&app, &machine).await,
        Cmd::Rename { machine, name } => ops::rename(&app, &machine, &name).await,
        Cmd::Rm { machine } => ops::rm(&app, &machine).await,
        Cmd::Expose { machine } => ops::set_exposure(&app, &machine, true).await,
        Cmd::Unexpose { machine } => ops::set_exposure(&app, &machine, false).await,
        Cmd::Sync => ops::sync(&app).await,
        Cmd::Doctor => ops::doctor(&app).await,
    }
}
