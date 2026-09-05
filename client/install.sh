#!/bin/sh
# omni (agent-side) — enrollment installer and client command in one file.
#
# First run (piped from the server):
#   curl -fsSL https://omni.tld/install.sh | sudo sh -s -- <token> [--with-herdr] [--name x] [--label x]
#
# The installer's last act is to write a persistent copy of itself to
# /usr/local/bin/omni. After that, everything is `omni <verb>` on the box:
#   omni status | restart | update | logs | uninstall
#
# A machine can enroll with several proxies (independent omni stacks): each
# enrollment lives under /etc/omni/proxies/<label>/ with its own client.toml
# and its own rathole process, because a rathole client dials exactly one
# remote_addr. Verbs act on every proxy unless given a label. The proxies
# never coordinate; each is a self-contained recovery path.
#
# POSIX sh. Must run on macOS bash 3.2 and dash. No jq, no GNU-only sed,
# no associative arrays, no mapfile, no ${var,,}.
set -eu

# Templated by the omni server when it serves this file.
OMNI_URL="__OMNI_URL__"
RATHOLE_VERSION="__RATHOLE_VERSION__"
HERDR_INSTALL_URL="__HERDR_INSTALL_URL__"

ETC_DIR=/etc/omni
PROXIES_DIR=$ETC_DIR/proxies
BIN_DIR=/usr/local/bin
RATHOLE_BIN=$BIN_DIR/rathole
SELF_BIN=$BIN_DIR/omni
SERVICE_PREFIX=omni-rathole
HB_SERVICE=omni-heartbeat
SYSTEMD_DIR=/etc/systemd/system
MAC_PLIST_DIR=/Library/LaunchDaemons
MAC_HB_PLIST=$MAC_PLIST_DIR/com.omni.heartbeat.plist

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

# ---------------------------------------------------------------- proxies ----

# Filesystem-, systemd-instance-, and launchd-label-safe name derived from
# the proxy URL host (override with --label at enroll).
label_from_url() {
    hostport=${1#*://}
    hostport=${hostport%%/*}
    printf '%s' "$hostport" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9._-' '-'
}

all_labels() {
    [ -d "$PROXIES_DIR" ] || return 0
    for d in "$PROXIES_DIR"/*/; do
        [ -f "$d/machine" ] || continue
        basename "$d"
    done
}

proxy_count() { all_labels | wc -l | tr -d ' '; }

# Sets m_id, m_secret, m_port, m_name, m_url, m_label; points OMNI_URL at
# this proxy.
load_state() {
    state="$PROXIES_DIR/$1/machine"
    [ -f "$state" ] || die "no proxy '$1' (have: $(all_labels | tr '\n' ' '))"
    # Shell-sourceable key='value' file written by ourselves at enroll time.
    . "$state"
    [ -n "${m_id:-}" ] && [ -n "${m_secret:-}" ] || die "$state is corrupt — re-enroll this proxy"
    OMNI_URL=${m_url:-$OMNI_URL}
}

# Verbs take an optional label: none means every proxy.
resolve_labels() {
    if [ -n "${1:-}" ]; then
        [ -f "$PROXIES_DIR/$1/machine" ] || die "no proxy '$1' (have: $(all_labels | tr '\n' ' '))"
        echo "$1"
        return 0
    fi
    labels=$(all_labels)
    [ -n "$labels" ] || die "not enrolled with any proxy — enroll first with a token"
    echo "$labels"
}

mac_plist() { echo "$MAC_PLIST_DIR/com.omni.rathole.$1.plist"; }
mac_log() { echo "/var/log/omni-rathole.$1.log"; }

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

# One shared binary serves every proxy's tunnel; when proxies pin different
# versions, `omni update` warns and the last-updated pin wins.
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

# The template and heartbeat units are shared; each proxy is an instance.
write_shared_units_linux() {
    cat > "$SYSTEMD_DIR/$SERVICE_PREFIX@.service" <<EOF
[Unit]
Description=omni rathole tunnel (%i)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$RATHOLE_BIN $PROXIES_DIR/%i/client.toml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    cat > "$SYSTEMD_DIR/$HB_SERVICE.service" <<EOF
[Unit]
Description=omni heartbeat (all proxies)

[Service]
Type=oneshot
ExecStart=$SELF_BIN heartbeat
EOF
    cat > "$SYSTEMD_DIR/$HB_SERVICE.timer" <<EOF
[Unit]
Description=omni heartbeat every 5 minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
EOF
    systemctl daemon-reload
    systemctl enable --now "$HB_SERVICE.timer" >/dev/null 2>&1 || true
}

enable_service_linux() {
    systemctl enable --now "$SERVICE_PREFIX@$1.service" >/dev/null 2>&1 \
        || systemctl restart "$SERVICE_PREFIX@$1.service"
}

write_services_darwin() {
    plist=$(mac_plist "$1")
    cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.omni.rathole.$1</string>
    <key>ProgramArguments</key><array>
        <string>$RATHOLE_BIN</string>
        <string>$PROXIES_DIR/$1/client.toml</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>$(mac_log "$1")</string>
    <key>StandardErrorPath</key><string>$(mac_log "$1")</string>
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
    chown root:wheel "$plist" "$MAC_HB_PLIST"
    chmod 0644 "$plist" "$MAC_HB_PLIST"
    launchctl bootout "system/com.omni.rathole.$1" 2>/dev/null || true
    launchctl bootstrap system "$plist" 2>/dev/null || launchctl load -w "$plist"
    launchctl bootout system/com.omni.heartbeat 2>/dev/null || true
    launchctl bootstrap system "$MAC_HB_PLIST" 2>/dev/null || launchctl load -w "$MAC_HB_PLIST"
}

service_restart() {
    case "$(os_type)" in
        linux) systemctl restart "$SERVICE_PREFIX@$1.service" ;;
        darwin) launchctl kickstart -k "system/com.omni.rathole.$1" 2>/dev/null || {
                    launchctl unload "$(mac_plist "$1")" 2>/dev/null || true
                    launchctl load -w "$(mac_plist "$1")"
                } ;;
    esac
}

service_active() {
    case "$(os_type)" in
        linux) systemctl is-active --quiet "$SERVICE_PREFIX@$1.service" ;;
        darwin) launchctl print "system/com.omni.rathole.$1" >/dev/null 2>&1 ;;
    esac
}

service_remove() {
    case "$(os_type)" in
        linux)
            systemctl disable --now "$SERVICE_PREFIX@$1.service" 2>/dev/null || true
            ;;
        darwin)
            launchctl bootout "system/com.omni.rathole.$1" 2>/dev/null || true
            rm -f "$(mac_plist "$1")" "$(mac_log "$1")"
            ;;
    esac
}

# ----------------------------------------------------------------- enroll ----

install_self() {
    tmp=$(mktemp)
    if curl -fsSL "$1/install.sh" -o "$tmp"; then
        install -m 0755 "$tmp" "$SELF_BIN"
        rm -f "$tmp"
        return 0
    fi
    rm -f "$tmp"
    return 1
}

write_state() {
    # $1 label — remaining values from m_* globals
    umask 077
    cat > "$PROXIES_DIR/$1/machine" <<EOF
m_id='$m_id'
m_secret='$m_secret'
m_port='$m_port'
m_name='${m_name:-}'
m_url='$OMNI_URL'
m_label='$1'
EOF
}

do_enroll() {
    need_baked_url
    need_root enroll "$@"
    token=""
    with_herdr=0
    name=$(uname -n)
    label=$(label_from_url "$OMNI_URL")
    while [ $# -gt 0 ]; do
        case "$1" in
            --with-herdr) with_herdr=1 ;;
            --name) shift; name=${1:-} ;;
            --label) shift; label=$(printf '%s' "${1:-}" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9._-' '-') ;;
            -*) die "unknown flag: $1" ;;
            *) token=$1 ;;
        esac
        shift
    done
    [ -n "$token" ] || die "usage: curl -fsSL $OMNI_URL/install.sh | sudo sh -s -- <token> [--with-herdr] [--name x] [--label x]"
    [ -n "$label" ] || die "empty --label"
    command -v curl >/dev/null 2>&1 || die "curl is required"
    check_sshd
    install_rathole

    say "enrolling with $OMNI_URL (proxy label: $label) ..."
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

    mkdir -p "$PROXIES_DIR/$label"
    chmod 0700 "$ETC_DIR" "$PROXIES_DIR" "$PROXIES_DIR/$label"
    umask 077
    printf '%s\n' "$resp" \
        | awk '/^---BEGIN CLIENT TOML---$/{f=1;next} /^---END CLIENT TOML---$/{f=0} f' \
        > "$PROXIES_DIR/$label/client.toml"
    [ -s "$PROXIES_DIR/$label/client.toml" ] || die "server response contained no client.toml"
    write_state "$label"

    case "$(os_type)" in
        linux)
            write_shared_units_linux
            enable_service_linux "$label"
            ;;
        darwin) write_services_darwin "$label" ;;
    esac

    install_self "$OMNI_URL" || say "warning: could not install the omni command to $SELF_BIN"

    if [ "$with_herdr" -eq 1 ]; then
        case "$HERDR_INSTALL_URL" in
            ""|__*) say "warning: --with-herdr requested but no Herdr install URL is configured on the server" ;;
            *) say "installing Herdr ..."
               curl -fsSL "$HERDR_INSTALL_URL" | sh || say "warning: Herdr install failed — the tunnel works without it" ;;
        esac
    fi

    say ""
    say "enrolled: $m_name (port $m_port on proxy '$label')"
    n=$(proxy_count)
    [ "$n" -gt 1 ] && say "this machine now tunnels to $n proxies: $(all_labels | tr '\n' ' ')"
    say "manage with: omni status | restart | update | logs | uninstall  (add a label to target one proxy)"
}

# ------------------------------------------------------------------ verbs ----

do_status() {
    labels=$(resolve_labels "${1:-}")
    for l in $labels; do
        (
            load_state "$l"
            say "proxy: $l ($OMNI_URL)"
            if service_active "$l"; then
                say "  tunnel:  up ($SERVICE_PREFIX@$l)"
            else
                say "  tunnel:  DOWN ($SERVICE_PREFIX@$l)"
            fi
            say "  machine: ${m_name:-?} (${m_id})"
            say "  port:    ${m_port:-?} on the proxy, forwarding to 127.0.0.1:22"
            if curl -fsS -m 5 -o /dev/null -X POST "$OMNI_URL/heartbeat" \
                -H "X-Omni-Machine: $m_id" -H "X-Omni-Secret: $m_secret" 2>/dev/null; then
                say "  server:  reachable"
            else
                say "  server:  UNREACHABLE"
            fi
        )
    done
}

do_restart() {
    need_root restart "$@"
    labels=$(resolve_labels "${1:-}")
    for l in $labels; do
        service_restart "$l"
        say "restarted $SERVICE_PREFIX@$l"
    done
}

do_logs() {
    labels=$(resolve_labels "${1:-}")
    case "$(os_type)" in
        linux)
            if [ -n "${1:-}" ]; then
                exec journalctl -u "$SERVICE_PREFIX@$1" -f
            fi
            exec journalctl -u "$SERVICE_PREFIX@*" -f
            ;;
        darwin)
            files=""
            for l in $labels; do
                files="$files $(mac_log "$l")"
            done
            # shellcheck disable=SC2086
            exec tail -f $files
            ;;
    esac
}

do_heartbeat() {
    ok=0
    fail=0
    for l in $(all_labels); do
        if (
            load_state "$l"
            curl -fsS -m 10 -o /dev/null -X POST "$OMNI_URL/heartbeat" \
                -H "X-Omni-Machine: $m_id" -H "X-Omni-Secret: $m_secret"
        ) 2>/dev/null; then
            ok=$((ok + 1))
        else
            fail=$((fail + 1))
        fi
    done
    # One proxy being down is the redundancy scenario, not a client fault;
    # only report failure when nothing was reachable.
    [ "$fail" -gt 0 ] && [ "$ok" -eq 0 ] && exit 1
    exit 0
}

do_update() {
    need_root update "$@"
    labels=$(resolve_labels "${1:-}")
    versions=""
    last_url=""
    for l in $labels; do
        # Subshell would lose variables we need; load in-place per iteration.
        load_state "$l"
        hdrs=$(mktemp)
        if ! body=$(curl -fsS -D "$hdrs" "$OMNI_URL/config" \
            -H "X-Omni-Machine: $m_id" -H "X-Omni-Secret: $m_secret"); then
            rm -f "$hdrs"
            say "warning: proxy '$l' unreachable — skipped"
            continue
        fi
        ver=$(awk -F': *' 'tolower($1)=="x-omni-rathole-version"{gsub(/\r/,"",$2);print $2}' "$hdrs")
        port=$(awk -F': *' 'tolower($1)=="x-omni-port"{gsub(/\r/,"",$2);print $2}' "$hdrs")
        nm=$(awk -F': *' 'tolower($1)=="x-omni-name"{gsub(/\r/,"",$2);print $2}' "$hdrs")
        rm -f "$hdrs"
        [ -n "$body" ] || { say "warning: proxy '$l' sent an empty config — skipped"; continue; }

        umask 077
        printf '%s\n' "$body" > "$PROXIES_DIR/$l/client.toml"
        [ -n "$port" ] && m_port=$port
        [ -n "$nm" ] && m_name=$nm
        write_state "$l"
        if [ -n "$ver" ]; then
            RATHOLE_VERSION=$ver
            versions="$versions $ver"
        fi
        install_rathole
        service_restart "$l"
        say "proxy '$l': config refreshed, tunnel restarted"
        last_url=$OMNI_URL
    done
    case "$versions" in
        *" "*" "*)
            first=${versions# }
            first=${first%% *}
            for v in $versions; do
                [ "$v" = "$first" ] || say "warning: proxies pin different rathole versions ($versions ) — last one won; align the pins"
            done
            ;;
    esac
    # Self-update last: install(1) replaces the file while this shell keeps
    # its own copy, so finishing the run is safe.
    if [ -n "$last_url" ]; then
        install_self "$last_url" || say "warning: could not refresh $SELF_BIN"
    fi
}

do_uninstall() {
    need_root uninstall "$@"
    if [ -n "${1:-}" ]; then
        l=$(resolve_labels "$1")
        say "removing proxy '$l' (other proxies and sshd are left alone) ..."
        service_remove "$l"
        rm -rf "${PROXIES_DIR:?}/$l"
        remaining=$(proxy_count)
        if [ "$remaining" -eq 0 ]; then
            say "that was the last proxy — run 'omni uninstall' (no label) to remove the tooling too"
        else
            say "done — still enrolled with: $(all_labels | tr '\n' ' ')"
        fi
        return 0
    fi
    say "uninstalling omni everywhere (sshd is left alone) ..."
    for l in $(all_labels); do
        service_remove "$l"
    done
    case "$(os_type)" in
        linux)
            systemctl disable --now "$HB_SERVICE.timer" 2>/dev/null || true
            rm -f "$SYSTEMD_DIR/$SERVICE_PREFIX@.service" \
                  "$SYSTEMD_DIR/$HB_SERVICE.service" \
                  "$SYSTEMD_DIR/$HB_SERVICE.timer"
            systemctl daemon-reload
            ;;
        darwin)
            launchctl bootout system/com.omni.heartbeat 2>/dev/null || true
            rm -f "$MAC_HB_PLIST"
            ;;
    esac
    rm -rf "$ETC_DIR"
    rm -f "$RATHOLE_BIN" "$SELF_BIN"
    say "done — tell each server with: omni rm <machine> (on the VPS)"
}

usage() {
    cat <<EOF
omni (agent-side)

enroll:    curl -fsSL <omni-url>/install.sh | sudo sh -s -- <token> [--with-herdr] [--name x] [--label x]
           (run one proxy's one-liner per proxy; enrollments are independent)
manage:    omni status [label]      tunnels up? which ports?
           omni restart [label]     bounce tunnel service(s)
           omni update [label]      re-fetch config, upgrade rathole to the pin
           omni logs [label]        tail the rathole log(s)
           omni uninstall [label]   remove one proxy, or everything with no label

with no label, verbs act on every enrolled proxy.
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
