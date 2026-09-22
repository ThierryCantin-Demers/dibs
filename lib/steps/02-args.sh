ORIG_ARGS=("$@")
SUBCOMMAND=0

while [ $# -gt 0 ]; do
    case "$1" in
        --bench|-b)   MODE=bench; shift ;;
        --shared)     MODE=shared; shift ;;
        --status|-s)  MODE=status; shift ;;
        --sync)       MODE=sync; shift; break ;;
        --rsh)        MODE=rsh; shift; break ;;
        --watch|-w)   MODE=watch; shift
                      if [ $# -gt 0 ] && [ -n "$1" ] && [ -z "${1//[0-9]/}" ]; then
                          WATCH_N=$1; shift
                      fi ;;
        --peek)       MODE=peek; shift ;;
        --hold)       HOLD=1; shift ;;
        --with)       n=${2:-}; n=${n%%=*}
                      case "${2:-}" in
                          [a-z]*=*) ;;
                          *) echo "dibs: --with takes name='<command>', as in --with server='./serve --port 7700'" >&2; exit 2 ;;
                      esac
                      case "$n" in *[!a-z0-9_]*)
                          echo "dibs: a --with name is lowercase letters, digits and _, and '$n' is not" >&2; exit 2 ;;
                      esac
                      case " ${WITH_NAME[*]} " in *" $n "*) echo "dibs: two services are called $n" >&2; exit 2 ;; esac
                      WITH_NAME+=("$n"); WITH_CMD+=("${2#*=}"); WITH_READY+=(""); shift 2 ;;
        --ready)      [ "${#WITH_NAME[@]}" -gt 0 ] ||
                          { echo "dibs: --ready says when the --with before it is ready, and none comes before it" >&2; exit 2; }
                      WITH_READY[-1]=${2:-}; shift 2 ;;
        --port)       n=${2:-}
                      case "$n" in
                          ''|[!a-z]*|*[!a-z0-9_]*)
                              echo "dibs: --port names a port for the machine to pick, as in --port api, which the" >&2
                              echo "  service and the command then read as \$DIBS_PORT_API. It does not ask for a" >&2
                              echo "  number: a number everyone writes down is the collision this avoids." >&2
                              exit 2 ;;
                      esac
                      case " ${PORT_NAME[*]} " in *" $n "*) echo "dibs: two ports are called $n" >&2; exit 2 ;; esac
                      PORT_NAME+=("$n"); shift 2 ;;
        --ready-within) READY_WITHIN=${2:-}
                      case "$READY_WITHIN" in ''|*[!0-9]*) echo "dibs: --ready-within takes seconds" >&2; exit 2 ;; esac
                      shift 2 ;;
        -v|--verbose) VERBOSE=1; shift ;;
        --json)       JSON=1; shift ;;
        --release)    MODE=release; shift ;;
        --gc)         MODE=gc; shift ;;
        --friction)   report_friction "${2:-}" ;;
        --dry-run)    DRY=1; shift ;;
        --days)       need --days "${2:-}"; GC_DAYS=$2; shift 2 ;;
        --log)        MODE=log; shift
                      if [ $# -gt 0 ] && [ -n "$1" ] && [ -z "${1//[0-9]/}" ]; then
                          LOG_N=$1; shift
                      fi ;;
        --on)         use_machine "${2:-}"; PINNED=1; HOSTFROM=--on; shift 2 ;;
        --any)        ROUTE=1; shift ;;
        --all)        ALL=1; shift ;;
        --forget)     need --forget "${2:-}"; MODE=forget; FORGET=$2; shift 2 ;;
        --prefer)     need --prefer "${2:-}"; PREFER=$2; shift 2 ;;
        --repo)       need --repo "${2:-}"; REPO=$2; shift 2 ;;
        --pick)       MODE=pick; shift ;;
        --registry-sync) MODE=registry; shift ;;
        --update)     MODE=update; shift ;;
        --detach)     DETACH=1; shift ;;
        --jobs)       MODE=jobs; shift ;;
        --job)        need --job "${2:-}"; MODE=job; JOBID=$2; shift 2 ;;
        --cancel)     need --cancel "${2:-}"; MODE=cancel; JOBID=$2; shift 2 ;;
        --abi)        MODE=abi; shift ;;
        --which)      MODE=which; shift ;;
        --machines)   MODE=machines; shift ;;
        --write)      WRITE=1; shift ;;
        --check)      MODE=check; shift
                      if [ $# -gt 0 ] && [ -n "$1" ] && [ "${1#-}" = "$1" ]; then
                          # A name the inventory knows is that machine, reached the way every
                          # other command reaches it. Taken literally instead, it is a
                          # different host string: one that resolves elsewhere or not at all,
                          # and that nothing has ever recorded a host key for. The command
                          # whose job is to say whether a machine is usable was the one
                          # command that could not reach it.
                          #
                          # A name nobody has recorded is still taken literally, because that
                          # is what makes --check the thing that onboards a new machine.
                          if [ -n "$(inv "$1" ssh)" ]; then
                              use_machine "$1"
                          else
                              HOST=$1; TARGET=$1
                          fi
                          PINNED=1; HOSTFROM=--check; shift
                      fi ;;
        --out)        MODE=out; shift
                      if [ $# -gt 0 ] && [ -n "$1" ] && [ -z "${1//[0-9-]/}" ]; then
                          OUTPID=$1; shift
                      fi ;;
        --fetch)      MODE=fetch; FETCHJOB=${2:-}; shift; [ $# -gt 0 ] && shift
                      if [ $# -gt 0 ] && [ -n "$1" ] && [ "${1#-}" = "$1" ]; then
                          FETCHTO=$1; shift
                      fi ;;
        --kill)       need --kill "${2:-}"; MODE=kill; KILLPID=$2; shift 2 ;;
        --force)      FORCE=1; shift ;;
        --anyone)     ANYONE=1; shift ;;
        --wait)       WAIT=$2; shift 2 ;;
        --max)        MAXHOLD=$2; shift 2 ;;
        --device)     DEVICE=${2:-}; shift 2 ;;
        --new-series) NEW_SERIES=1; shift ;;
        --preflight)  PREFLIGHT=1; shift ;;
        --stream)     STREAM=1; shift ;;
        --label)      LABEL=$2; shift 2 ;;
        -h|--help)    usage ;;
        --)           shift; break ;;
        -*)           echo "unknown option: $1" >&2; usage 2 ;;
        *)
            # A subcommand is the first word that is not a flag, so flags may come before it.
            [ "$SUBCOMMAND" = 0 ] || break
            SUBCOMMAND=1
            case "$1" in
                run)    [ "$MODE" = shared ] || [ "$MODE" = bench ] || break; shift ;;
                status) [ "$MODE" = shared ] || break; MODE=status; shift ;;
                gc)     [ "$MODE" = shared ] || break; MODE=gc; shift ;;
                friction) [ "$MODE" = shared ] || break; report_friction "${2:-}" ;;
                out)    [ "$MODE" = shared ] || break; MODE=out; shift
                        if [ $# -gt 0 ] && [ -n "$1" ] && [ -z "${1//[0-9-]/}" ]; then
                            OUTPID=$1; shift
                        fi ;;
                fetch)  [ "$MODE" = shared ] || break; MODE=fetch; FETCHJOB=${2:-}; shift; [ $# -gt 0 ] && shift
                        if [ $# -gt 0 ] && [ -n "$1" ] && [ "${1#-}" = "$1" ]; then
                            FETCHTO=$1; shift
                        fi ;;
                build|test|bench|list|runs|gaps|shell|raw|batch|with)
                    # The recipe layer takes its own flags, after the verb. --on is the one that
                    # can come first, and it travels as DIBS_ON.
                    consumed=$(( ${#ORIG_ARGS[@]} - $# ))
                    if [ "$consumed" -gt 0 ] && ! { [ "$HOSTFROM" = --on ] && [ "$consumed" -eq 2 ]; }; then
                        echo "dibs: only --on can come before $1. Put the rest after it:  dibs $1 ... <flags>" >&2
                        exit 2
                    fi
                    [ "$HOSTFROM" = --on ] && export DIBS_ON=$MACHINE
                    [ "$1" = batch ] && export DIBS_BATCH_OWNER=$(agent_id)
                    version_notice
                    core=$(dibs_core) ||
                        { echo "dibs: $1 needs the recipe layer, which is not installed. Run install.sh from the clone." >&2; exit 2; }
                    exec "$core" "$@" ;;
                *)      break ;;
            esac ;;
    esac
done
