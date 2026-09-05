# omni

An enrollment service. Run one command on a new machine; thirty seconds later
it's in [Termix](https://github.com/LukeGus/Termix) in the browser, on any
device. omni is not in the data path — it writes config for
[rathole](https://github.com/rathole-org/rathole) and Termix, which are.

The full design is in [PLAN.md](PLAN.md). Layout:

```
server/   omni server — Rust. Runs on the VPS only. Holds SQLite, tokens, the Termix API key.
client/   install.sh — POSIX sh installer that doubles as the on-box `omni` command. Served by the server.
deploy/   VPS as code: compose file (rathole + Termix), omni.service, example config.
```

## Build & deploy (VPS)

```sh
cd server && cargo build --release
sudo install -m 0755 target/release/omni /usr/local/bin/omni
sudo install -m 0600 -D deploy/omni.example.toml /etc/omni/omni.toml   # edit it
sudo install -m 0644 deploy/omni.service /etc/systemd/system/omni.service
docker compose -f deploy/docker-compose.yml up -d
sudo systemctl enable --now omni
omni doctor
```

## Use

```sh
omni token new --name laptop
# prints:  curl -fsSL https://omni.tld/install.sh | sudo sh -s -- <token>
# run that on the new machine; it appears in Termix.
omni ls
```

On an enrolled machine: `omni status | restart | update | logs | uninstall`.

A machine can enroll with several independent proxies (run each proxy's
one-liner; `--label x` names it). Each gets its own state dir under
`/etc/omni/proxies/<label>/` and its own rathole process; verbs act on every
proxy unless given a label, and `omni uninstall <label>` removes just that one.
The proxies never know about each other.

The Termix integration is verified against Termix-SSH/Termix main (Sept 2026):
omni creates hosts via `POST /host/enroll` with a `tmx_…` API key minted in the
Termix UI, and updates are read-modify-write so toggles you flip in the UI
survive a rename or sync. Details in `server/src/termix.rs`.

The rathole pin is verified too: v0.5.0 is the newest release (checked Sept
2026), the zip holds a single `rathole` binary, and `rapiz1/rathole` on GitHub
and Docker Hub is the same project as `rathole-org` (redirect). Per-platform:
Linux x86_64 ships gnu-only, arm64 Linux is musl, a Windows msvc build exists
for the eventual PowerShell client, and there is **no arm64 macOS build** — the
installer falls back to the x86_64 one under Rosetta 2 and says so.

The Herdr questions are answered from herdr.dev docs (Sept 2026). "Direct
attach" is `herdr agent attach <name>` — your terminal connects to one agent's
terminal without the full sidebar UI. PLAN.md's claim holds: direct attach is
the one Windows-only gap, and a Windows box also can't be a `herdr --remote`
target. But native Windows Herdr is GA (ConPTY, no WSL), and full-UI attach
works from a PowerShell session — so the path on an enrolled Windows machine
is: SSH in over the tunnel, run `herdr`, use the full UI; skip direct attach
and inbound `--remote` there. The official installer omni's `--with-herdr`
pipes is `https://herdr.dev/install.sh` (Windows twin: `install.ps1`, for
phase 4).

## Unverified assumptions

Check these before trusting them (they match PLAN.md's "verify before
building" list):

- **Termix container** — env vars in `deploy/docker-compose.yml` (listen port,
  data path) follow the image docs loosely; confirm against the image.
- Termix full-screen URL route on cold mobile open, and Herdr pane sizing with
  laptop and phone attached at once — still open; both need the live services.
