out_kept() {   # dir
    cat "$1/head"
    printf '  kept on this computer: %s  (%s bytes, last %s lines)\n' "$1/log" "$(wc -c < "$1/log")" "$OUT_N"
    tail -n "$OUT_N" "$1/log" | sed 's/^/  | /'
}
