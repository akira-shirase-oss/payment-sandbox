#!/usr/bin/env bash
# The CLI quickstart from docs/DEVELOPERS.md, run as written by CI against a real sandbox.
# Needs a running sandbox and one of your project's admin keys:
#   export PS_API_URL=http://127.0.0.1:56080 PS_API_KEY=sk_sim_...
# SIMULATED MONEY ONLY: no real cards, balances or banking connection.
set -euo pipefail

sandbox-cli status

# A purchase: hold 42.00, capture it, refund 12.00.
payment=$(sandbox-cli --json payments create --amount 42.00 --currency USD --description "Order 1001" | jq -r .payment.id)
sandbox-cli payments capture "$payment" --amount 42.00
sandbox-cli payments refund "$payment" --amount 12.00

# Every balance is explained by journal lines, and the ledger reconciles.
sandbox-cli ledger explain USD merchant_pending
sandbox-cli reconcile

# A lost reply: exit status 4 means "not known yet", never "paid".
set +e
lost=$(sandbox-cli --json payments create --amount 7.00 --currency USD --scenario lost_reply)
status=$?
set -e
[ "$status" -eq 4 ]
sandbox-cli payments wait "$(jq -r .payment.id <<<"$lost")"

# A hosted checkout: a link for the payer (it holds the session's secret), then its state.
session=$(sandbox-cli --json checkout create --amount 25.00 --currency USD --description "Order 1004" | jq -r .session.id)
sandbox-cli checkout get "$session"

# Replay the failure library and check every trace against the committed one.
sandbox-cli scenarios run --all --check
