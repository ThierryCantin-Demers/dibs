#!/usr/bin/env bash
# Put dibs on this machine. Run it from the clone; run it again after a pull if you want
# the recipe layer rebuilt. dibs --update does both.
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

# The recipe layer goes under libexec, off PATH: dibs build, test and bench reach it, and nothing
# else should. Without cargo you still have a working lock, so this is a warning.
if command -v cargo >/dev/null 2>&1; then
    # The commit is stamped into the binary, so dibs --update can tell a stale build from a
    # current one.
    CORE=${PREFIX:-$HOME/.local}/libexec/dibs
    DIBS_CORE_COMMIT=$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown) \
        cargo install --quiet --path "$HERE/core" --root "$CORE" --force
    echo "installed the recipe layer $(git -C "$HERE" rev-parse --short HEAD 2>/dev/null) in $CORE/bin"
    cargo install --quiet --path "$HERE/dibs-tui" --root "${PREFIX:-$HOME/.local}" --force
    echo "installed dibstop in $BIN"
else
    echo "no cargo, so the recipe layer and dibstop were not built. Install Rust and run this again, or ask" >&2
    echo "whoever owns the machine for a prebuilt dibs-core to drop in ${PREFIX:-$HOME/.local}/libexec/dibs/bin." >&2
fi

case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo; echo "$BIN is not on your PATH. Add it:" >&2
       echo "  bash/zsh   echo 'export PATH=\"\$PATH:$BIN\"' >> ~/.bashrc" >&2
       echo "  fish       fish_add_path $BIN" >&2 ;;
esac

# The machine is not in the script, on purpose: one clone works against any of them.
if [ -z "${DIBS_HOST:-}" ] && [ ! -s "${DIBS_MACHINES:-${XDG_CONFIG_HOME:-$HOME/.config}/dibs/machines.toml}" ]; then
    echo
    echo "Record the machine you were given, and every call can reach it:"
    echo "  dibs --check dibs@<machine> --write"
fi
