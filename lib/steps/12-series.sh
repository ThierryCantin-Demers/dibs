# A label is the key a measurement's history is filed under, so two runs of one label are
# meant to be two samples of one thing. They are not, if they ran on different silicon: a
# number from one card cannot be compared with a number from another, and nothing about the
# two numbers says so. Each machine keeps its own series of a label, since the machine is named
# on every call and in every record; within one, a run on another card than the series' is
# refused, because that is usually a missing --device and nothing else would show it.
#
# Only benchmarks. A build does not care which card it did not use, and blocking one would
# make this an obstacle rather than a guard.
SERIES=${DIBS_SERIES:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/series}
# Stamped, and ignored wholesale when the stamp is not this one. What a machine is keyed by
# has changed once already, and a key change turns every existing record into an apparent
# move: without this, one edit here refuses every benchmark anyone has ever run, which is a
# far worse failure than forgetting where a label last ran.
# A line is label, machine, card, who, when and, since per-machine series, how many runs.
SERIES_V='#dibs-series 1'

# Checked before the run and recorded after it: a job that never measured anything must not
# claim the label. A deliberate move starts the series again rather than appending to it,
# since filing the new numbers beside the old ones rebuilds the mixed history this exists to
# refuse.
if [ "$MODE" = bench ] && [ "$HOLD" = 0 ] && [ "${DIBS_SERIES_CHECK:-1}" = 1 ] && [ "$NEW_SERIES" != 1 ]; then
    series_check || exit 2
fi
# The recipe layer asks this before it builds, so a measurement refused here is refused before
# minutes of building under the shared lock rather than after them.
[ "$PREFLIGHT" = 1 ] && exit 0
