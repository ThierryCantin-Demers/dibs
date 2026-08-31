#!/usr/bin/env bash
# The half of the dibs tests that needs a real ssh channel: a job has to die with the
# caller that started it, and that cannot be shown without one.
#
# It takes the lock and kills things, so it refuses to run unless the machine is idle and you
# have said so out loud:
#
#   tests/dibs-live-test.sh --machine-is-mine
#
# Everything else is covered by dibs-test.sh, which never leaves this machine.

T=${DIBS:-${DIBS_BIN:-$HOME/.local/bin/dibs}}
pass=0; fail=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1"; pass=$((pass+1));
          else echo "  FAIL $1: expected [$3], got [$2]"; fail=$((fail+1)); fi; }

[ "${1:-}" = "--machine-is-mine" ] || {
    echo "Refusing to run: this takes the lock on a shared machine." >&2
    echo "Check 'dibs --status', then rerun with --machine-is-mine." >&2
    exit 2
}
state=$($T --status 2>&1) || { echo "$state" >&2; exit 69; }
grep -q 'dibs: idle' <<<"$state" || {
    echo "Refusing to run: the machine is in use." >&2
    echo "$state" >&2
    exit 75
}

echo "a job dies with the caller that started it"
$T --label live-hangup 'python3 -c "sum(range(4000000000))"' >/dev/null 2>&1 &
LOCAL=$!
for i in $(seq 400); do $T --status 2>/dev/null | grep -q live-hangup && break; done
H=$($T --status 2>/dev/null | awk '/live-hangup/{for(i=1;i<=NF;i++) if($i=="pid") print $(i+1)}' | head -1)
check "it is holding the lock" "$([ -n "$H" ] && echo yes || echo no)" "yes"
tree=$($T --peek "ps -eo pid=,ppid=,args= | awk -v r=$H 'BEGIN{w[r]=1} {p[NR]=\$1;q[NR]=\$2;\$1=\"\";\$2=\"\";a[NR]=\$0} END{do{c=0;for(i=1;i<=NR;i++) if(w[q[i]]&&!w[p[i]]){w[p[i]]=1;c=1}}while(c); for(i=1;i<=NR;i++) if(w[p[i]]) print p[i], substr(a[i],1,50)}'" 2>/dev/null)
WORK=$(awk '/python3 -c/{print $1}' <<<"$tree" | tail -1)
check "its workload is running over there" "$([ -n "$WORK" ] && echo yes || echo no)" "yes"

kill -9 $LOCAL 2>/dev/null; wait $LOCAL 2>/dev/null
gone=no
for i in $(seq 20); do
  [ "$($T --peek "ps -p $WORK > /dev/null 2>&1 && echo alive || echo gone" 2>/dev/null | tr -d ' \r')" = gone ] \
    && { gone=yes; break; }
done
check "the workload died with it" "$gone" "yes"
check "the lock came back" "$($T --status 2>&1 | grep -c 'dibs: idle')" "1"

echo "a caller that dies while its job is queued takes it out of the queue"
F=/tmp/dibs-live.$$
$T --peek "rm -f $F /tmp/dibs-live-marker; mkfifo $F" >/dev/null 2>&1
$T --bench --max 60 --label live-holder "read -r _ < $F" >/dev/null 2>&1 & HOLD=$!
for i in $(seq 400); do $T --status 2>/dev/null | grep -q live-holder && break; done
$T --label live-queued "echo ran > /tmp/dibs-live-marker" >/dev/null 2>&1 & QUEUED=$!
for i in $(seq 400); do $T --status 2>/dev/null | grep -q live-queued && break; done
check "it is queued behind the benchmark" "$($T --status | grep -c live-queued)" "1"
kill -9 $QUEUED 2>/dev/null; wait $QUEUED 2>/dev/null
left=no
for i in $(seq 20); do $T --status 2>/dev/null | grep -q live-queued || { left=yes; break; }; done
check "killing its caller drops it from the queue" "$left" "yes"
check "and it never ran" \
  "$($T --peek 'test -e /tmp/dibs-live-marker && echo ran || echo never' 2>/dev/null | tr -d ' \r')" "never"
check "the log says why" "$($T --log 20 2>/dev/null | grep -c 'caller-gone.*live-queued')" "1"
$T --peek "printf 'go\n' > $F" >/dev/null 2>&1 &
wait $HOLD 2>/dev/null
$T --peek "rm -f $F /tmp/dibs-live-marker" >/dev/null 2>&1

echo "--watch dies with the terminal that was watching"
WOUT=/tmp/dibs-live-watch.$$
$T --watch 2 > "$WOUT" 2>&1 & W=$!
for i in $(seq 400); do grep -q 'ctrl-c to stop' "$WOUT" 2>/dev/null && break; done
remote_watches() { $T --peek "ps -eo args= | grep -c '[.]dibs-run[^ ]* watch'" 2>/dev/null | tr -d ' \r'; }
check "the loop is running over there" "$(remote_watches)" "1"
kill -9 $W 2>/dev/null; wait $W 2>/dev/null
left=no
for i in $(seq 20); do [ "$(remote_watches)" = 0 ] && { left=yes; break; }; done
check "and it stops when the caller does" "$left" "yes"
rm -f "$WOUT"

echo "rsync reaches the machine through the lock"
D=/tmp/dibs-live-sync.$$
R=~/.cache/dibs/live-sync-$$
mkdir -p "$D" && echo one > "$D/a.txt" && head -c 20000 /dev/urandom > "$D/b.bin"
$T --sync -a "$D/" ":$R/" >/dev/null 2>&1
check "a tree lands over there" "$($T --peek "cat $R/a.txt" 2>/dev/null | tr -d ' \r')" "one"
$T --peek "touch $R/stale" >/dev/null 2>&1
$T --sync -a --delete "$D/" ":$R/" >/dev/null 2>&1
check "--delete takes away what is no longer here" \
  "$($T --peek "ls $R" 2>/dev/null | tr -d ' \r' | sort | tr '\n' ' ')" "a.txt b.bin "
$T --sync -a ":$R/" "$D/back/" >/dev/null 2>&1
check "and the whole tree comes back byte for byte" \
  "$(diff -r "$D/a.txt" "$D/back/a.txt" >/dev/null && cmp -s "$D/b.bin" "$D/back/b.bin" && echo same)" "same"
check "it was a job like any other" \
  "$([ "$($T --log 30 | grep -c ' sync ')" -ge 1 ] && echo yes || echo no)" "yes"
$T --peek "rm -rf $R" >/dev/null 2>&1
rm -rf "$D"

echo "and the ordinary paths still work over ssh"
check "a run returns its output" "$($T --label live-run 'echo hello-over-ssh' 2>/dev/null)" "hello-over-ssh"
check "--peek needs no lock" "$($T --peek 'echo peeked' 2>/dev/null)" "peeked"
check "no tty, so git does not page" \
  "$($T --peek 'cd "$HOME" && ls -d . >/dev/null; echo returned' 2>/dev/null | tail -1)" "returned"
check "the log recorded the run" "$($T --log 20 2>/dev/null | grep -c live-run)" "2"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
