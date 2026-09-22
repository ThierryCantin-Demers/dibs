if [ "$MODE" != update ]; then
    version_notice
else
    self=$(readlink -f "$0")
    clone=$(cd "$(dirname "$self")/.." && pwd)
    if ! git -C "$clone" rev-parse --git-dir >/dev/null 2>&1; then
        echo "dibs: $self is not inside a git clone, so there is nothing to pull." >&2
        echo "  A --copy install cannot update itself. Install from a clone:  ./install.sh" >&2
        exit 2
    fi
    before=$(git -C "$clone" rev-parse --short HEAD)
    # git replaces files rather than rewriting them, so this running copy keeps reading the
    # old inode and the pull cannot corrupt the script mid-execution.
    if ! git -C "$clone" pull --ff-only --quiet; then
        echo "dibs: could not fast-forward $clone, so nothing was updated." >&2
        exit 1
    fi
    after=$(git -C "$clone" rev-parse --short HEAD)
    if [ "$before" = "$after" ]; then
        echo "dibs $after, already current"
    else
        echo "dibs $before -> $after"
        git -C "$clone" log --oneline "$before..$after" | sed 's/^/  /'
    fi
    installed=$("$(dibs_core)" --version 2>/dev/null | sed -n 's/.*(\(.*\)).*/\1/p')
    if [ "$before" != "$after" ] || [ "$installed" != "$after" ]; then
        bash "$clone/install.sh" || { echo "dibs: install.sh failed; the clone is at $after" >&2; exit 1; }
    fi
    recipes=${DIBS_RECIPES:-${XDG_CONFIG_HOME:-$HOME/.config}/dibs/recipes}
    if git -C "$recipes" rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1; then
        rb=$(git -C "$recipes" rev-parse --short HEAD)
        git -C "$recipes" pull --ff-only --quiet ||
            { echo "dibs: could not fast-forward the recipes in $recipes" >&2; exit 1; }
        ra=$(git -C "$recipes" rev-parse --short HEAD)
        if [ "$rb" = "$ra" ]; then echo "recipes $ra, already current"
        else echo "recipes $rb -> $ra"; git -C "$recipes" log --oneline "$rb..$ra" | sed 's/^/  /'; fi
    elif [ -d "$recipes" ]; then
        echo "recipes in $recipes are not a clone with an upstream, so they were left alone"
    fi
    version_notice quiet
    exit 0
fi
