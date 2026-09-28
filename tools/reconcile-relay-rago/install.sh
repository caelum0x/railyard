#!/usr/bin/env bash
# install.sh: install the nightly Relay and Rago reconciliation on box 2 (RY-RG1).
#   sudo ./install.sh            # units + env sample, enable the timer
#   sudo ./install.sh run        # one run now for yesterday, then show the log
# The code runs from this checkout (RAILYARD_SRC, default /srv/src/railyard).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="${RAILYARD_SRC:-/srv/src/railyard}"
UNIT_DIR="${UNIT_DIR:-/etc/systemd/system}"
ETC="${PRODUCTS_ETC:-/etc/products}"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }

case "${1:-install}" in
  install)
    for unit in reconcile-relay-rago.service reconcile-relay-rago.timer; do
      sed "s#/srv/src/railyard#$SRC#g" "$HERE/systemd/$unit" >"$UNIT_DIR/$unit"
      chmod 644 "$UNIT_DIR/$unit"
    done
    [[ -f "$ETC/reconcile-relay-rago.env" ]] || install -m 640 "$HERE/reconcile.env.sample" "$ETC/reconcile-relay-rago.env"
    systemctl daemon-reload
    systemctl enable --now reconcile-relay-rago.timer
    systemctl list-timers reconcile-relay-rago.timer --no-pager
    ;;
  run)
    systemctl start reconcile-relay-rago.service
    journalctl -u reconcile-relay-rago.service -n 20 --no-pager
    ;;
  *) sed -n '2,5p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
