#!/bin/sh
# Runs opencode fully isolated: every file it writes goes under $SPIKE_DIR, never the home dir.
# usage: SPIKE_DIR=<dir> launch.sh <config.json> <opencode arguments…>
set -eu
SPIKE_DIR=${SPIKE_DIR:?set SPIKE_DIR to a scratch directory}
CONFIG=$1
shift
export XDG_CONFIG_HOME="$SPIKE_DIR/xdg/config" XDG_DATA_HOME="$SPIKE_DIR/xdg/data"
export XDG_STATE_HOME="$SPIKE_DIR/xdg/state" XDG_CACHE_HOME="$SPIKE_DIR/xdg/cache"
export OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DISABLE_CLAUDE_CODE=1 OPENCODE_CONFIG="$CONFIG"
exec opencode "$@"
