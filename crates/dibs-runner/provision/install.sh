#!/bin/sh
# Builds this tree's runner and installs it where a call for the hash given looks for it.
set -e
hash=$1
DIBS_RUNNER_HASH=$hash cargo build --locked --release
built=${CARGO_TARGET_DIR:-target}/release/dibs-runner
named=$("$built" hash) || true
if [ "$named" != "$hash" ]; then
    echo "dibs-runner: cargo left the runner of ${named:-no source} where the runner of $hash should be, so it was not installed." >&2
    exit 1
fi
dest="$HOME/.cache/dibs/runner/$hash"
mkdir -p "$dest"
cp "$built" "$dest/.dibs-runner.$$"
mv -f "$dest/.dibs-runner.$$" "$dest/dibs-runner"
