# The recipe layer prepares a tree and sends it in one job: DIBS_SYNC_BEFORE names a script that
# runs on the machine ahead of the transfer, under the same lock, and leaves it in the directory
# the transfer names. A script that cannot be read refuses the transfer, since without it the
# transfer would land wherever the job started.
sync_before() {
    [ -n "${DIBS_SYNC_BEFORE:-}" ] || return 0
    [ -s "$DIBS_SYNC_BEFORE" ] || { echo "dibs: DIBS_SYNC_BEFORE names $DIBS_SYNC_BEFORE, which cannot be read" >&2; return 1; }
    cat "$DIBS_SYNC_BEFORE"
}
