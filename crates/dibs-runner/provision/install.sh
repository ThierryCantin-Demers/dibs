#!/bin/sh
# Builds this tree's runner and installs it where a call for the hash given looks for it.
set -e
hash=$1
cargo build --locked --release
dest="$HOME/.cache/dibs/runner/$hash"
mkdir -p "$dest"
cp "${CARGO_TARGET_DIR:-target}/release/dibs-runner" "$dest/.dibs-runner.$$"
mv -f "$dest/.dibs-runner.$$" "$dest/dibs-runner"
