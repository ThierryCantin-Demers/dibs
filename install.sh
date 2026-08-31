#!/usr/bin/env bash
# Put dibs on this machine. Run it from the clone; run it again after a pull if you want
# dibs-run rebuilt.
#
#   ./install.sh
#
# Symlinks rather than copies, so a pull updates the tool without a second step. Pass --copy
# if you would rather have files that do not move under you.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=${PREFIX:-$HOME/.local}/bin
MODE=symlink
[ "${1:-}" = --copy ] && MODE=copy

mkdir -p "$BIN"
for f in dibs; do
    if [ "$MODE" = copy ]; then install -m 755 "$HERE/bin/$f" "$BIN/$f"
    else ln -sfn "$HERE/bin/$f" "$BIN/$f"; fi
done
echo "installed dibs in $BIN"

# dibs-run is the interface; dibs underneath it is only the lock. Without cargo you still have
# a working machine, so this is a warning rather than a failure.
if command -v cargo >/dev/null 2>&1; then
    cargo install --quiet --path "$HERE/dibs-run" --root "${PREFIX:-$HOME/.local}" --force
    echo "installed dibs-run in $BIN"
    cargo install --quiet --path "$HERE/dibs-tui" --root "${PREFIX:-$HOME/.local}" --force
    echo "installed dibstop in $BIN"
else
    echo "no cargo, so dibs-run and dibstop were not built. Install Rust and run this again, or ask" >&2
    echo "whoever owns the machine for a prebuilt binary to drop in $BIN." >&2
fi

case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo; echo "$BIN is not on your PATH. Add it:" >&2
       echo "  bash/zsh   echo 'export PATH=\"\$PATH:$BIN\"' >> ~/.bashrc" >&2
       echo "  fish       fish_add_path $BIN" >&2 ;;
esac

# The machine is not in the script, on purpose: one clone works against any of them.
if [ -z "${DIBS_HOST:-}" ]; then
    echo
    echo "Set DIBS_HOST to the machine you were given. There is no default, so every call"
    echo "fails until you do:"
    echo "  bash/zsh   echo 'export DIBS_HOST=dibs@<machine>' >> ~/.bashrc"
    echo "  fish       set -Ux DIBS_HOST dibs@<machine>"
fi
