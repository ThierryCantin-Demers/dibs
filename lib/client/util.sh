lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

is_batch_id() { [[ $1 =~ ^[0-9]{8}-[0-9]{6}-[0-9]+$ ]]; }

usage() { sed -n '2,/^# --- end of the interface/p' "$0" | sed '$d; s/^# \?//'; exit "${1:-0}"; }

# A flag whose value is missing must not be left where it is: `shift 2` with one argument
# left shifts nothing and returns, so the loop reads the same flag again and never ends. That
# is a hang the caller cannot interrupt, from a typo.
need() {   # flag value
    [ -n "$2" ] || { echo "dibs: $1 needs a value." >&2; exit 2; }
}
