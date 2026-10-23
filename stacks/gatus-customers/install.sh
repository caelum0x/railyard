#!/usr/bin/env bash
# install.sh: install Railyard Watch's checks agent + its Gatus probe engine
# on box 1 (PY2.2).
#   sudo ./install.sh            # units + env sample, start gatus, enable the agent
#   sudo ./install.sh run        # one agent pass now, then show the log
#   sudo ./install.sh status     # unit + container status
# The code runs from this checkout (RAILYARD_SRC, default /srv/src/railyard).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="${RAILYARD_SRC:-/srv/src/railyard}"
UNIT_DIR="${UNIT_DIR:-/etc/systemd/system}"
ETC="${PRODUCTS_ETC:-/etc/products}"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 1; }
command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

case "${1:-install}" in
  install)
    sed "s#/srv/src/railyard#$SRC#g" "$HERE/systemd/watch-agent.service" >"$UNIT_DIR/watch-agent.service"
    chmod 644 "$UNIT_DIR/watch-agent.service"
    [[ -f "$ETC/watch-agent.env" ]] || install -m 640 "$HERE/agent.env.sample" "$ETC/watch-agent.env"
    echo "edit $ETC/watch-agent.env (WATCH_CONSOLE_URL, WATCH_AGENT_TOKEN) before enabling, if you have not already"
    systemctl daemon-reload
    systemctl enable --now watch-agent.service
    systemctl status watch-agent.service --no-pager -l || true
    ;;
  run)
    systemctl start watch-agent.service || true
    "$HERE/agent.sh" once
    ;;
  status)
    systemctl status watch-agent.service --no-pager -l || true
    docker compose -f "$HERE/docker-compose.yml" ps
    ;;
  *) sed -n '2,6p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
