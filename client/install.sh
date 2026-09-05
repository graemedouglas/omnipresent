#!/bin/sh
# omni (agent-side) — enrollment installer and client command in one file.
#
# First run (piped from the server):
#   curl -fsSL https://omni.tld/install.sh | sudo sh -s -- <token> [--with-herdr] [--name x]
#
# The installer's last act is to write a persistent copy of itself to
# /usr/local/bin/omni. After that, everything is `omni <verb>` on the box:
#   omni status | restart | update | logs | uninstall
#
# POSIX sh. Must run on macOS bash 3.2 and dash. No jq, no GNU-only sed,
# no associative arrays, no mapfile, no ${var,,}.
set -eu

# Templated by the omni server when it serves this file.
OMNI_URL="__OMNI_URL__"
RATHOLE_VERSION="__RATHOLE_VERSION__"
HERDR_INSTALL_URL="__HERDR_INSTALL_URL__"

ETC_DIR=/etc/omni
STATE_FILE=$ETC_DIR/machine
CLIENT_TOML=$ETC_DIR/client.toml
BIN_DIR=/usr/local/bin
RATHOLE_BIN=$BIN_DIR/rathole
SELF_BIN=$BIN_DIR/omni
SERVICE=omni-rathole
HB_SERVICE=omni-heartbeat
MAC_RATHOLE_PLIST=/Library/LaunchDaemons/com.omni.rathole.plist
MAC_HB_PLIST=/Library/LaunchDaemons/com.omni.heartbeat.plist
MAC_LOG=/var/log/omni-rathole.log

say() { printf '%s\n' "$*"; }
die() { printf 'omni: %s\n' "$*" >&2; exit 1; }

os_type() {
    case "$(uname -s)" in
        Linux) echo linux ;;
        Darwin) echo darwin ;;
        *) die "unsupported OS: $(uname -s)" ;;
    esac
}

need_root() {
    [ "$(id -u)" -eq 0 ] && return 0
    # Re-exec under sudo when we exist as a real file; when piped there is
    # no file to re-exec, so tell the user to add sudo to the one-liner.
    if [ -f "$0" ] && [ -x "$0" ]; then
        exec sudo -- "$0" "$@"
    fi
    die "must run as root — re-run with:  curl -fsSL $OMNI_URL/install.sh | sudo sh -s -- <token>"
}

need_baked_url() {
    case "$OMNI_URL" in
        __*) die "this copy has no server baked in — fetch it from your omni server: curl -fsSL https://<omni>/install.sh" ;;
    esac
}

load_state() {
    [ -f "$STATE_FILE" ] || die "not enrolled (no $STATE_FILE) — enroll first with a token"
    # Shell-sourceable key='value' file written by ourselves at enroll time.
    . "$STATE_FILE"
    [ -n "${m_id:-}" ] && [ -n "${m_secret:-}" ] || die "$STATE_FILE is corrupt — re-enroll"
    # Prefer the URL recorded at enroll over whatever is baked into this copy.
    OMNI_URL=${m_url:-$OMNI_URL}
}

# ---------------------------------------------------------------- rathole ----

# Candidate release targets in preference order, verified against the
# v0.5.0 asset list. Linux x86_64 ships gnu-only; macOS arm64 has no native
# build at v0.5.0 and runs the x86_64 one under Rosetta 2 (the aarch64 name
# stays first so a future release that adds it wins automatically).
rathole_targets() {
    arch=$(uname -m)
    case "$(os_type)-$arch" in
        linux-x86_64)               echo "x86_64-unknown-linux-gnu x86_64-unknown-linux-musl" ;;
        linux-aarch64|linux-arm64)  echo "aarch64-unknown-linux-musl" ;;
        linux-armv7l|linux-armv6l)  echo "armv7-unknown-linux-musleabihf" ;;
        darwin-x86_64)              echo "x86_64-apple-darwin" ;;
        darwin-arm64|darwin-aarch64) echo "aarch64-apple-darwin x86_64-apple-darwin" ;;
        *) die "no rathole build for $(os_type)/$arch" ;;
    esac
}

# `rathole --version` prints a multi-line build report; the version is on
# the "Build Version:" line.
rathole_installed_version() {
    "$RATHOLE_BIN" --version 2>/dev/null | awk '/Build Version:/{print $3; exit}'
}

install_rathole() {
    current=$(rathole_installed_version) || current=""
    if [ "$current" = "$RATHOLE_VERSION" ]; then
        say "rathole v$RATHOLE_VERSION already installed"
        return 0
    fi
    command -v unzip >/dev/null 2>&1 || die "unzip is required to unpack rathole — install it and re-run"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    found=""
    for target in $(rathole_targets); do
        url="https://github.com/rathole-org/rathole/releases/download/v$RATHOLE_VERSION/rathole-$target.zip"
        if curl -fsSL -o "$tmp/rathole.zip" "$url" 2>/dev/null; then
            found=$target
            break
        fi
    done
    [ -n "$found" ] || die "could not download rathole v$RATHOLE_VERSION for: $(rathole_targets)"
    say "downloaded rathole-$found.zip (v$RATHOLE_VERSION)"
    case "$(os_type)-$(uname -m)-$found" in
        darwin-arm64-x86_64-*|darwin-aarch64-x86_64-*)
            say "note: no arm64 macOS build at v$RATHOLE_VERSION — using the x86_64 one via Rosetta 2"
            say "      (if it won't start: softwareupdate --install-rosetta)"
            ;;
    esac
    unzip -oq "$tmp/rathole.zip" -d "$tmp"
    [ -f "$tmp/rathole" ] || die "rathole binary missing from release archive"
    install -m 0755 "$tmp/rathole" "$RATHOLE_BIN"
    say "installed rathole v$RATHOLE_VERSION -> $RATHOLE_BIN"
}

# ------------------------------------------------------------------- sshd ----

check_sshd() {
    case "$(os_type)" in
        linux)
            if command -v systemctl >/dev/null 2>&1; then
                systemctl is-active --quiet ssh 2>/dev/null && return 0
                systemctl is-active --quiet sshd 2>/dev/null && return 0
            fi
            pgrep -x sshd >/dev/null 2>&1 && return 0
            die "sshd is not running — install and enable OpenSSH server (e.g. apt install openssh-server; systemctl enable --now ssh), then re-run"
            ;;
        darwin)
            launchctl print system/com.openssh.sshd >/dev/null 2>&1 && return 0
            die "Remote Login is off — enable it: System Settings > General > Sharing > Remote Login (or: sudo systemsetup -setremotelogin on), then re-run"
            ;;
    esac
}

# --------------------------------------------------------------- services ----

write_services_linux() {
    cat > "/etc/systemd/system/$SERVICE.service" <<EOF
[Unit]
Description=omni rathole tunnel
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$RATHOLE_BIN $CLIENT_TOML
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    cat > "/etc/systemd/system/$HB_SERVICE.service" <<EOF
[Unit]
Description=omni heartbeat

[Service]
Type=oneshot
ExecStart=$SELF_BIN heartbeat
EOF
    cat > "/etc/systemd/system/$HB_SERVICE.timer" <<EOF
[Unit]
Description=omni heartbeat every 5 minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
EOF
    systemctl daemon-reload
    systemctl enable --now "$SERVICE.service" >/dev/null 2>&1 || systemctl restart "$SERVICE.service"
    systemctl enable --now "$HB_SERVICE.timer" >/dev/null 2>&1 || true
}

write_services_darwin() {
    cat > "$MAC_RATHOLE_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.omni.rathole</string>
    <key>ProgramArguments</key><array>
        <string>$RATHOLE_BIN</string>
        <string>$CLIENT_TOML</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>$MAC_LOG</string>
    <key>StandardErrorPath</key><string>$MAC_LOG</string>
</dict></plist>
EOF
    cat > "$MAC_HB_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.omni.heartbeat</string>
    <key>ProgramArguments</key><array>
        <string>$SELF_BIN</string>
        <string>heartbeat</string>
    </array>
    <key>StartInterval</key><integer>300</integer>
</dict></plist>
EOF
    chown root:wheel "$MAC_RATHOLE_PLIST" "$MAC_HB_PLIST"
    chmod 0644 "$MAC_RATHOLE_PLIST" "$MAC_HB_PLIST"
    launchctl bootout system/com.omni.rathole 2>/dev/null || true
    launchctl bootstrap system "$MAC_RATHOLE_PLIST" 2>/dev/null || launchctl load -w "$MAC_RATHOLE_PLIST"
    launchctl bootout system/com.omni.heartbeat 2>/dev/null || true
    launchctl bootstrap system "$MAC_HB_PLIST" 2>/dev/null || launchctl load -w "$MAC_HB_PLIST"
}

service_restart() {
    case "$(os_type)" in
        linux) systemctl restart "$SERVICE.service" ;;
        darwin) launchctl kickstart -k system/com.omni.rathole 2>/dev/null || {
                    launchctl unload "$MAC_RATHOLE_PLIST" 2>/dev/null || true
                    launchctl load -w "$MAC_RATHOLE_PLIST"
                } ;;
    esac
}

service_active() {
    case "$(os_type)" in
        linux) systemctl is-active --quiet "$SERVICE.service" ;;
        darwin) launchctl print system/com.omni.rathole >/dev/null 2>&1 ;;
    esac
}

# ----------------------------------------------------------------- enroll ----

install_self() {
    tmp=$(mktemp)
    if curl -fsSL "$OMNI_URL/install.sh" -o "$tmp"; then
        install -m 0755 "$tmp" "$SELF_BIN"
        rm -f "$tmp"
        return 0
    fi
    rm -f "$tmp"
    return 1
}

do_enroll() {
    need_baked_url
    need_root enroll "$@"
    token=""
    with_herdr=0
    name=$(uname -n)
    while [ $# -gt 0 ]; do
        case "$1" in
            --with-herdr) with_herdr=1 ;;
            --name) shift; name=${1:-} ;;
            -*) die "unknown flag: $1" ;;
            *) token=$1 ;;
        esac
        shift
    done
    [ -n "$token" ] || die "usage: curl -fsSL $OMNI_URL/install.sh | sudo sh -s -- <token> [--with-herdr] [--name x]"
    command -v curl >/dev/null 2>&1 || die "curl is required"
    check_sshd
    install_rathole

    say "enrolling with $OMNI_URL ..."
    resp=$(curl -fsS -X POST "$OMNI_URL/enroll" \
        --data-urlencode "token=$token" \
        --data-urlencode "name=$name" \
        --data-urlencode "os=$(os_type)") \
        || die "enrollment failed — token expired, already used, or server unreachable"

    case "$resp" in
        OMNI-ENROLL-V1*) ;;
        *) die "unexpected response from server" ;;
    esac
    m_id=$(printf '%s\n' "$resp" | sed -n 's/^machine_id=//p')
    m_secret=$(printf '%s\n' "$resp" | sed -n 's/^machine_secret=//p')
    m_port=$(printf '%s\n' "$resp" | sed -n 's/^port=//p')
    m_name=$(printf '%s\n' "$resp" | sed -n 's/^name=//p')
    [ -n "$m_id" ] && [ -n "$m_secret" ] && [ -n "$m_port" ] || die "incomplete enrollment response"

    mkdir -p "$ETC_DIR"
    chmod 0700 "$ETC_DIR"
    umask 077
    printf '%s\n' "$resp" \
        | awk '/^---BEGIN CLIENT TOML---$/{f=1;next} /^---END CLIENT TOML---$/{f=0} f' \
        > "$CLIENT_TOML"
    [ -s "$CLIENT_TOML" ] || die "server response contained no client.toml"
    cat > "$STATE_FILE" <<EOF
m_id='$m_id'
m_secret='$m_secret'
m_port='$m_port'
m_name='$m_name'
m_url='$OMNI_URL'
EOF

    case "$(os_type)" in
        linux) write_services_linux ;;
        darwin) write_services_darwin ;;
    esac

    install_self || say "warning: could not install the omni command to $SELF_BIN"

    if [ "$with_herdr" -eq 1 ]; then
        case "$HERDR_INSTALL_URL" in
            ""|__*) say "warning: --with-herdr requested but no Herdr install URL is configured on the server" ;;
            *) say "installing Herdr ..."
               curl -fsSL "$HERDR_INSTALL_URL" | sh || say "warning: Herdr install failed — the tunnel works without it" ;;
        esac
    fi

    say ""
    say "enrolled: $m_name (port $m_port on the server)"
    say "tunnel service: $SERVICE — it should appear in Termix within seconds"
    say "manage this machine with: omni status | restart | update | logs | uninstall"
}

# ------------------------------------------------------------------ verbs ----

do_status() {
    load_state
    if service_active; then
        say "tunnel:  up ($SERVICE)"
    else
        say "tunnel:  DOWN ($SERVICE)"
    fi
    say "machine: ${m_name:-?} (${m_id})"
    say "port:    ${m_port:-?} on the server, forwarding to 127.0.0.1:22"
    if curl -fsS -m 5 -o /dev/null -X POST "$OMNI_URL/heartbeat" \
        -H "X-Omni-Machine: $m_id" -H "X-Omni-Secret: $m_secret" 2>/dev/null; then
        say "server:  reachable ($OMNI_URL)"
    else
        say "server:  UNREACHABLE ($OMNI_URL)"
    fi
    case "$(os_type)" in
        linux) say "recent:" ; journalctl -u "$SERVICE" --no-pager -n 3 -o cat 2>/dev/null | sed 's/^/  /' || true ;;
        darwin) say "recent:" ; tail -n 3 "$MAC_LOG" 2>/dev/null | sed 's/^/  /' || true ;;
    esac
}

do_restart() {
    need_root restart
    load_state
    service_restart
    say "restarted $SERVICE"
}

do_logs() {
    case "$(os_type)" in
        linux) exec journalctl -u "$SERVICE" -f ;;
        darwin) exec tail -f "$MAC_LOG" ;;
    esac
}

do_heartbeat() {
    load_state
    curl -fsS -m 10 -o /dev/null -X POST "$OMNI_URL/heartbeat" \
        -H "X-Omni-Machine: $m_id" -H "X-Omni-Secret: $m_secret"
}

do_update() {
    need_root update
    load_state
    hdrs=$(mktemp)
    body=$(curl -fsS -D "$hdrs" "$OMNI_URL/config" \
        -H "X-Omni-Machine: $m_id" -H "X-Omni-Secret: $m_secret") \
        || { rm -f "$hdrs"; die "could not fetch config from $OMNI_URL"; }
    ver=$(awk -F': *' 'tolower($1)=="x-omni-rathole-version"{gsub(/\r/,"",$2);print $2}' "$hdrs")
    port=$(awk -F': *' 'tolower($1)=="x-omni-port"{gsub(/\r/,"",$2);print $2}' "$hdrs")
    nm=$(awk -F': *' 'tolower($1)=="x-omni-name"{gsub(/\r/,"",$2);print $2}' "$hdrs")
    rm -f "$hdrs"
    [ -n "$body" ] || die "server returned an empty config"

    umask 077
    printf '%s\n' "$body" > "$CLIENT_TOML"
    [ -n "$port" ] && m_port=$port
    [ -n "$nm" ] && m_name=$nm
    cat > "$STATE_FILE" <<EOF
m_id='$m_id'
m_secret='$m_secret'
m_port='$m_port'
m_name='${m_name:-}'
m_url='$OMNI_URL'
EOF
    [ -n "$ver" ] && RATHOLE_VERSION=$ver
    install_rathole
    service_restart
    say "config refreshed, rathole at v$RATHOLE_VERSION, tunnel restarted"
    # Self-update last: install(1) replaces the file while this shell keeps
    # its own copy, so finishing the run is safe.
    install_self || say "warning: could not refresh $SELF_BIN"
}

do_uninstall() {
    need_root uninstall
    say "uninstalling omni (sshd is left alone) ..."
    case "$(os_type)" in
        linux)
            systemctl disable --now "$HB_SERVICE.timer" 2>/dev/null || true
            systemctl disable --now "$SERVICE.service" 2>/dev/null || true
            rm -f "/etc/systemd/system/$SERVICE.service" \
                  "/etc/systemd/system/$HB_SERVICE.service" \
                  "/etc/systemd/system/$HB_SERVICE.timer"
            systemctl daemon-reload
            ;;
        darwin)
            launchctl bootout system/com.omni.heartbeat 2>/dev/null || true
            launchctl bootout system/com.omni.rathole 2>/dev/null || true
            rm -f "$MAC_RATHOLE_PLIST" "$MAC_HB_PLIST" "$MAC_LOG"
            ;;
    esac
    rm -rf "$ETC_DIR"
    rm -f "$RATHOLE_BIN" "$SELF_BIN"
    say "done — tell the server with: omni rm <machine> (on the VPS)"
}

usage() {
    cat <<EOF
omni (agent-side)

enroll:    curl -fsSL <omni-url>/install.sh | sudo sh -s -- <token> [--with-herdr] [--name x]
manage:    omni status      tunnel up? which port?
           omni restart     bounce the tunnel service
           omni update      re-fetch config, upgrade rathole to the server's pin
           omni logs        tail the rathole log
           omni uninstall   stop, remove, leave sshd alone
EOF
}

cmd=${1:-help}
case "$cmd" in
    status)    shift; do_status "$@" ;;
    restart)   shift; do_restart "$@" ;;
    logs)      shift; do_logs "$@" ;;
    update)    shift; do_update "$@" ;;
    uninstall) shift; do_uninstall "$@" ;;
    heartbeat) shift; do_heartbeat "$@" ;;
    enroll)    shift; do_enroll "$@" ;;
    help|-h|--help) usage ;;
    *) do_enroll "$@" ;;
esac
