#!/usr/bin/env bash
# The local stack: `up` starts PostgreSQL (compose project `payment-sandbox`, port 56432 on
# 127.0.0.1) and waits until it is healthy; `down` stops it; `reset` also deletes its volume;
# `env` prints the variables the server and the tests read; then run
# `sandbox-server setup` and `sandbox-server migrate` once (D-038).
set -euo pipefail
cd "$(dirname "$0")/.."

compose=(docker compose -f deploy/compose.yaml)
port="${PS_PG_PORT:-56432}"

case "${1:-}" in
  up)
    "${compose[@]}" up -d --wait postgres
    echo "stack ok: postgres on 127.0.0.1:${port}"
    ;;
  down)
    "${compose[@]}" down
    ;;
  reset)
    "${compose[@]}" down --volumes
    ;;
  env)
    # Three roles (D-038): the stack's superuser only creates roles and databases
    # (`sandbox-server setup`, the tests); ps_owner migrates; the server is ps_app.
    echo "export PS_ADMIN_DATABASE_URL=postgres://sandbox:sandbox-dev@127.0.0.1:${port}/sandbox"
    echo "export PS_MIGRATE_DATABASE_URL=postgres://ps_owner:ps-owner-dev@127.0.0.1:${port}/payment_sandbox"
    echo "export PS_DATABASE_URL=postgres://ps_app:ps-app-dev@127.0.0.1:${port}/payment_sandbox"
    echo "export PS_TEST_DATABASE_URL=postgres://sandbox:sandbox-dev@127.0.0.1:${port}/sandbox"
    ;;
  *)
    echo "usage: scripts/stack.sh up|down|reset|env" >&2
    exit 2
    ;;
esac
