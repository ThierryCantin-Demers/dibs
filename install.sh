#!/usr/bin/env bash
# Put dibs on this computer. Run it from the clone; dibs --update runs it again after a pull.
#
#   ./install.sh [--machines]
#
# dibs and dibstop are built from this clone and installed in $PREFIX/bin, ~/.local/bin unless
# PREFIX says otherwise. --machines adds dibs-machines, the desktop window on your machines.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PREFIX=${PREFIX:-$HOME/.local}
BIN=$PREFIX/bin
WINDOW=0
for a in "$@"; do
    case "$a" in
        --machines) WINDOW=1 ;;
        *) echo "install.sh: unknown option $a; it takes --machines" >&2; exit 2 ;;
    esac
done
# dibs --update passes nothing, so a window installed once is rebuilt from then on.
[ -x "$BIN/dibs-machines" ] && WINDOW=1

if ! command -v cargo >/dev/null 2>&1; then
    echo "install.sh: dibs is built with cargo, and there is none on PATH. Install Rust from" >&2
    echo "  https://rustup.rs and run this again." >&2
    exit 1
fi

mkdir -p "$BIN"
STAGE=$(mktemp -d "$PREFIX/.dibs-install.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
# The commit is stamped into the binary, so dibs --update can tell a stale build from a current one.
COMMIT=$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)
DIBS_COMMIT=$COMMIT cargo install --quiet --locked --path "$HERE/crates/dibs" --root "$STAGE" --force
# Renamed over dibs in one step, so a symlink to the bash dibs, or a dibs running now, is never
# seen half replaced.
mv -f "$STAGE/bin/dibs" "$BIN/.dibs.new"
mv -f "$BIN/.dibs.new" "$BIN/dibs"
rm -rf "$PREFIX/libexec/dibs"
rm -f "$BIN/dibs-run"
echo "installed dibs $COMMIT in $BIN"

cargo install --quiet --locked --path "$HERE/crates/dibstop" --root "$PREFIX" --force
echo "installed dibstop in $BIN"
if [ "$WINDOW" = 1 ]; then
    cargo install --quiet --locked --path "$HERE/crates/dibs-machines" --root "$PREFIX" --force
    echo "installed dibs-machines in $BIN"
fi

case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo; echo "$BIN is not on your PATH. Add it:" >&2
       echo "  bash       echo 'export PATH=\"\$PATH:$BIN\"' >> ~/.bashrc" >&2
       echo "  zsh        echo 'export PATH=\"\$PATH:$BIN\"' >> ~/.zshrc" >&2
       echo "  fish       fish_add_path $BIN" >&2 ;;
esac

# The machine is not in the script, on purpose: one clone works against any of them.
if [ -z "${DIBS_HOST:-}" ] && [ ! -s "${DIBS_MACHINES:-${XDG_CONFIG_HOME:-$HOME/.config}/dibs/machines.toml}" ]; then
    echo
    echo "Record the machine you were given, and every call can reach it:"
    echo "  dibs --check dibs@<machine> --write"
fi
