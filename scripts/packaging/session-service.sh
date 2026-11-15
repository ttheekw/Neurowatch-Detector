# Embedded in native package hooks. The package owns the default.target link;
# no machine-wide enable/disable state or user configuration is rewritten.
nw_detect_sessions() {
    command -v systemctl >/dev/null 2>&1 || return 0
    command -v runuser >/dev/null 2>&1 || return 0
    [ "$(id -u)" = 0 ] || return 0
    for nw_runtime in /run/user/[0-9]*; do
        [ -d "$nw_runtime" ] && [ -S "$nw_runtime/bus" ] || continue
        nw_uid="${nw_runtime##*/}"
        case "$nw_uid" in ''|*[!0-9]*) continue ;; esac
        [ "$nw_uid" -ne 0 ] || continue
        [ "$(stat -c %u "$nw_runtime")" = "$nw_uid" ] || continue
        nw_user="$(getent passwd "$nw_uid" | cut -d: -f1)"
        [ -n "$nw_user" ] || continue
        # Use the user's own manager: its sockets and identity stay private to
        # the same account as NeuroWatch. Containers without sessions skip this.
        nw_detect_userctl() {
            runuser -u "$nw_user" -- env \
                XDG_RUNTIME_DIR="$nw_runtime" \
                DBUS_SESSION_BUS_ADDRESS="unix:path=$nw_runtime/bus" \
                systemctl --user "$@"
        }
        nw_detect_userctl daemon-reload || continue
        case "$1" in
            install)
                nw_detect_userctl try-restart nw-detect.service || true
                nw_detect_userctl start nw-detect.service || true
                ;;
            stop) nw_detect_userctl stop nw-detect.service || true ;;
            reload) : ;;
        esac
    done
    return 0
}
