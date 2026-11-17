#!/usr/bin/env bash
# Screenshots of the screens the interface standard asks for (360 px and 1440 px, light and
# dark), written to docs/screens/ by the journeys that reach each state. CI never writes
# them; this script does, on demand, and the PNGs are committed. SIMULATED MONEY ONLY.
#
#   scripts/screens.sh                  every journey that takes screenshots
#   scripts/screens.sh tests/x.spec.ts  only these
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p docs/screens
specs=("$@")
[ "${#specs[@]}" -gt 0 ] || specs=(tests/screens.spec.ts tests/dataset.spec.ts)
E2E_SCREENS_DIR="$PWD/docs/screens" scripts/e2e.sh "${specs[@]}"
echo "screens ok: $(ls docs/screens | wc -l | tr -d ' ') files in docs/screens"
