# What differs between operating systems is behind the functions in PLATFORM_API, which each
# lib/machine/01-platform-<name>.sh defines for its own. Everything else assumes bash 5 and a GNU
# userland, which that file arranges where the system's own tools are something else.

# macOS's own bash is 3.2. This much parses in it, and bash reads a script one command at a time,
# so the rest of the file is never read by it.
if [ "${BASH_VERSINFO[0]}" -lt 5 ] && [ -z "${DIBS_REEXEC:-}" ]; then
    case "$0" in
        /*) for b in /opt/homebrew/bin/bash /usr/local/bin/bash; do
                [ -x "$b" ] && DIBS_REEXEC=1 exec "$b" "$0"
            done ;;
    esac
fi

case "$(uname -s)" in
    Linux) PLATFORM=linux ;;
    Darwin) PLATFORM=darwin ;;
    *) echo "dibs: $(uname -s) is not a platform dibs runs on yet: it needs a lib/machine/01-platform-<name>.sh" >&2
       exit 2 ;;
esac

PLATFORM_API="alive proc_state started_at pgid_of children_of fd_path tree_cpu_ticks
    counts_reaped_children lock_openers holds_flock ports_listening load_x100
    cpu_model os_name abi_facts has_battery pci_chip shared_lock_howto gpu_report gpu_entries
    machine_state"
