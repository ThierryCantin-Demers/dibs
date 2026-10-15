#!/bin/sh
# Builds this tree's dibs and installs it where a detached batch for the hash given looks for it.
set -e
hash=$1
# Not --locked: the lock was cut from a larger workspace's, and cargo trims it to this one,
# keeping every version it names.
cargo build --release -p dibs
built=${CARGO_TARGET_DIR:-target}/release/dibs
named=$("$built" __client-hash) || true
if [ "$named" != "$hash" ]; then
    echo "dibs: cargo left the dibs of ${named:-no source} where the dibs of $hash should be, so it was not installed." >&2
    exit 1
fi
dest="$HOME/.cache/dibs/client/$hash"
mkdir -p "$dest"
cp "$built" "$dest/.dibs.$$"
mv -f "$dest/.dibs.$$" "$dest/dibs"
