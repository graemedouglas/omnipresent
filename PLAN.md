# omni — build plan

## What it is

An enrollment service. Run one command on a new machine; thirty seconds later it's
in Termix in the browser, on any device.

omni is not in the data path. It writes config for tools that are.

## Pieces

| Thing | Runs where | Who wrote it |
|---|---|---|
| rathole server | VPS | not us |
| rathole client | each machine | not us |
| Termix (+ guacd) | VPS | not us |
| Herdr | each machine | not us |
| sshd | each machine | not us |
| **omni server** | VPS | us — Rust |
| **omni client** | each machine | us — bash + PowerShell |

## Data model

`machines`
- `id` — uuid, stable forever
- `name` — display name, what you see in Termix
- `port` — deterministic, pinned to id, never reused
- `service_token` — rathole per-service token, long-lived
- `machine_secret` — authenticates this machine back to omni, long-lived
- `bind_addr` — `127.0.0.1` default, `0.0.0.0` opt-in per machine
- `termix_host_id` — for idempotent updates
- `os`, `enrolled_at`, `last_seen`

`tokens`
- single-use, short expiry, hashed at rest

## Enrollment flow

1. You generate a token in omni (CLI or web form).
2. On the new machine: `curl -fsSL https://omni.tld/install.sh | sh -s -- <token>`
3. Installer: downloads rathole, POSTs the token to omni, gets back a machine id
   and a `client.toml`, writes a service unit, starts it.
4. omni appends a service stanza to `server.toml`. rathole hot-reloads. No restart.
5. omni POSTs to the Termix API to create the host pointing at `127.0.0.1:<port>`.
6. Machine appears in Termix.

Re-running enroll on an existing machine updates it. Never creates a duplicate.

## Three secrets, three jobs

**Enrollment token** — single-use, ~15 min expiry. Proves the person running the
install script is you. Consumed at step 3, then dead. Short-lived because it's the
one secret that travels loosely: it goes in a pasted shell command and ends up in
shell history and scrollback.

**Service token** — rathole's per-service credential. Long-lived. The tunnel
re-authenticates with it on every reconnect, forever. Never leaves the machine's
config file. Revoke by deleting the machine's stanza from `server.toml`; the tunnel
dies on next reconnect and no other machine is affected.

**Machine secret** — authenticates `/config` and `/heartbeat` calls back to omni.
Long-lived. Separate from the service token because it's a different service.

Connections are not short. Nothing expires out from under a running machine.

## Dependencies

omni installs what it needs, checks what it depends on, offers what's merely useful.

**Install — rathole.** Fetch the binary, verify checksum, place it. If the installer
said "first go download rathole" the one-command experience is gone, which was the
whole point.

**Check — sshd.** It's the tunnel's target and it's already present on every Linux
box and Mac worth enrolling (macOS just needs Remote Login enabled). Detect it; if
missing, fail with a message naming what to turn on. Do not install or configure it
— too much variance across distros, and it's a security-critical config we didn't
author.

**Offer — Herdr.** Optional flag or prompt. The tunnel and browser terminal work
fine without it; you just get a plain shell instead of persistent agent sessions.
Optional because not every machine runs coding agents, it has its own installer and
update channel, and it's the fastest-moving of the three.

**On the VPS**, assume rathole and Termix are already there (Phase 0 set them up).
omni checks they're reachable at startup and refuses to run with a clear error if
not. Installing them is not its job.

**Version pinning.** omni pins a rathole version; re-running enroll upgrades to the
pin. Otherwise the fleet drifts across versions and a config format change bites.

## Endpoints

```
POST /enroll          token -> machine id + secret + client.toml
GET  /config          machine auth -> current client.toml
POST /heartbeat       machine auth -> ok
GET  /install.sh
GET  /install.ps1
```

Config is fetched, not pushed. A machine that's been off for a month picks up
changes on next boot.

Liveness comes from rathole's own connection state where possible — omni already
knows which services have a client attached. `/heartbeat` is the fallback if that
turns out to be awkward to read.

## VPS layout

Phase 0's real output. Write it as a compose file or a short provisioning script,
not a sequence of things you did once. "How do I rebuild the VPS" is the question
that bites in eighteen months.

Ports:

| Port | Exposure | What |
|---|---|---|
| 2333 | public | rathole control — agents dial in. Firewalled, non-default port to cut scanner noise. |
| omni | public or own tunnel | install script + `/enroll` + `/config`. No Access policy — curl can't do a browser login. |
| Termix | loopback | cloudflared reaches it locally. Nothing web-facing on the firewall. |
| 22 | public, key-only | your own admin access to the VPS. |

Everything else closed. Forwarded machine ports (2201+) bind loopback and are only
reachable by Termix on the same box.

Optional: Cloudflare tunnel fronting Termix, with Access in front of that. Buys no
inbound web port and a second gate ahead of Termix's own login. Doesn't touch
rathole or omni. Add or remove it freely.

Back up: the omni SQLite DB and the Termix DB. Together they are the fleet.

## Two artifacts

They share a name and nothing else. Built from separate sources, never coexist on
one machine.

| | Server | Client |
|---|---|---|
| Name | `omni` | `omni` (agent-side) |
| Runs on | VPS only | every enrolled machine |
| Language | Rust | bash + PowerShell twin |
| Holds | SQLite, tokens, Termix API key | nothing |

Don't merge them. Merging ships the server's whole surface — including the code
that touches the host database — to every company laptop, and forces a Windows port
of server logic that will never run there.

## Server verbs

On the VPS:

```
omni serve                      run the HTTP service
omni token new [--name x]       mint an enrollment token, print the one-liner
omni ls                         machines: name, port, os, connected, last seen
omni show <machine>             detail
omni rename <machine> <name>    updates Termix host too
omni rm <machine>               revoke: drop stanza, delete Termix host
omni expose <machine>           bind 0.0.0.0 — direct SSH, phone clients
omni unexpose <machine>         back to loopback
omni sync                       reconcile server.toml + Termix against the DB
omni doctor                     rathole reachable? Termix API? config writable?
```

`sync` and `doctor` exist because config drift is the failure mode you'll actually
hit — a hand-edited `server.toml`, a Termix host deleted in the UI.

## Client verbs

The install one-liner's last act is to write a persistent copy of itself. Nothing
runs unless you type it — still no resident process, but a resident *command*:

```
omni status      tunnel up? which port? last connect?
omni restart     bounce the service
omni update      re-fetch config, upgrade rathole to the pin
omni logs        tail the rathole journal
omni uninstall   stop, remove, leave sshd alone
```

A thin wrapper over systemctl and journalctl plus one curl. ~100 lines. Without it,
a down tunnel means squinting at `journalctl` — exactly the friction omni exists to
remove.

First enrollment only:

```
curl -fsSL https://omni.tld/install.sh | sh -s -- <token> [--with-herdr]
```

Everything after that is `omni <verb>` on the box.

## Phases

**0 — prove it by hand.** No code. rathole server and one client configured
manually, Termix host added by hand, Herdr on the target. Open it on your phone.
Confirm the whole path works before automating any of it.

**1 — proxy skeleton.** Rust. SQLite. Token generation, `/enroll`, writes
`server.toml`, deterministic port assignment. Test with curl.

**2 — bash client.** POSIX sh, works on macOS bash 3.2. Installs rathole, writes a
service unit, and leaves a persistent copy of itself for the client verbs.
Idempotent.

**3 — Termix integration.** API key, create/update host on enroll. This closes
the loop and is the moment omni becomes worth having.

**4 — PowerShell client.** Only when there's a Windows box you care about. Service
registration is the work, not the language.

**5 — health.** Heartbeat, mark stale machines in Termix so a dead tunnel doesn't
look like a broken connection.

## Decided

- **rathole over frp** — per-service tokens (company and personal boxes isolated),
  hot reload, TCP_NODELAY on by default.
- **VPS, not home server** — the proxy is the recovery path; don't put it behind
  the outage. Home server enrolls as just another agent.
- **Ports on loopback** — only Termix reaches them. Nothing SSH-facing is public.
- **omni stays out of the data path** — swappable transport, small blast radius.
- **Persistence is Herdr's job**, not omni's. No replay buffers, no session
  protocol, no terminal emulation.

## Constraints

- macOS ships bash 3.2. No associative arrays, no `mapfile`, no `${var,,}`.
- Client tools: `curl`, `install`, builtins. No `jq`, no GNU-only `sed`/`readlink`.
- rathole's config watcher ignores symlinks — write the real file in place.
- Never a static shared secret across machines. See Three secrets.
- Back up the Termix DB. It holds every host and credential.

## Verify before building

- Termix full-screen URL route — does it survive a cold link-open on mobile, or
  bounce to login first?
- rathole Windows binary on the release page.
- Herdr sizing with laptop and phone attached at once.
- Herdr direct attach on Windows is not supported — confirm what the path is.

## Not doing

- Writing a proxy. Three mature ones exist.
- Writing a web terminal. Termix has auth, mobile apps, recording, audit.
- Public SSH ports. Revisit only if the browser terminal disappoints on mobile.
