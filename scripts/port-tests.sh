#!/usr/bin/env bash
# Run the black-box acceptance suite (tx-ide/port-tests) against this port.
#   scripts/port-tests.sh                 # every area
#   scripts/port-tests.sh home model      # only test_home.py + test_model.py
#   TX_REF=/path/to/tx-ide scripts/port-tests.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ref="${TX_REF:-$root/../tx-ide}"
[ -d "$ref/port-tests" ] || { echo "port-tests not found under $ref (set TX_REF)" >&2; exit 2; }

cargo build --quiet --manifest-path "$root/Cargo.toml" --bin tx
ln -sf "$root/target/debug/tx" "$root/bin/tx"

export TX_BIN="$root/bin/tx" TX_IMPL=rust TX_HELPERS_DIR="$root/bin"
# macOS: the default $TMPDIR pushes tmux socket paths past sun_path's 104 bytes.
export TMPDIR=/tmp

cd "$ref"
if [ $# -eq 0 ]; then
  exec python3.14 -m unittest discover port-tests -v
fi
status=0
for area in "$@"; do
  python3.14 -m unittest discover port-tests -v -p "test_${area}.py" || status=1
done
exit "$status"
