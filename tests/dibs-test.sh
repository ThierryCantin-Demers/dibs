#!/usr/bin/env bash
# The lock protocol, on this laptop only. Never contacts a real machine: every case runs with
# DIBS_LOCAL=1 against a scratch lock directory, so it is safe while agents are working.
#
# Holders block on a fifo, so they cost no CPU and release exactly when told. Waits are
# builtin-only and tightly bounded, so a regression fails in a second instead of spinning
# for minutes. Anything needing a real ssh channel lives in dibs-live-test.sh.

export DIBS_LOCAL=1
S=$(mktemp -d "${TMPDIR:-/tmp}/dibs-test.XXXXXX")
# Holders block on a fifo under $S, and removing the directory does not release them: they
# stay parked on a read that can never complete until their own --max expires, hours later.
# Matched on $S, which is unique to this run, so a suite running beside this one is untouched.
trap 'pkill -f "$S" 2>/dev/null; rm -rf "$S"' EXIT
export DIBS_LOCK_DIR=$S/lockdir DIBS_HISTORY=$S/history DIBS_LOG=$S/log
# Every piece of state the wrapper writes has to be redirected here, not only the ones a test
# reads back: a benchmark in this suite wrote its series into the real one, under the labels
# of whoever was using the machine.
export DIBS_SERIES=$S/series
mkdir -p "$DIBS_LOCK_DIR"
T=${DIBS:-${DIBS_BIN:-$HOME/.local/bin/dibs}}
DIBS_LOCK_DIR_SAVED=$DIBS_LOCK_DIR
pass=0; fail=0

check() { if [ "$2" = "$3" ]; then pass=$((pass+1));
          else echo "  FAIL $1: expected [$3], got [$2]"; fail=$((fail+1)); fi; }
fifo()  { rm -f "$S/f-$1"; mkfifo "$S/f-$1"; }
hold()  { echo "read -r _ < $S/f-$1"; }
free()  { timeout 5 bash -c "printf 'go\n' > '$S/f-$1'" 2>/dev/null; }
# Blocks until the fixture says it reached a point, at no CPU cost and with a bound.
sync_() { timeout 30 bash -c "read -r _ < '$S/f-$1'" 2>/dev/null; }
# Builtin-only and bounded: a glob and a test per turn, no subprocess, and a cap so a
# condition that never comes costs a moment rather than a minute.
count_() { local k=$1 f n=0; for f in "$DIBS_LOCK_DIR"/$k.*; do [ -e "$f" ] && n=$((n+1)); done; echo "$n"; }
holders() { count_ holder; }
# For conditions that need a real command each turn, so the cap is low on purpose.
until_() { local i=0; until eval "$1"; do i=$((i+1)); [ "$i" -gt 60 ] && return 1; done; }
waiters() { count_ waiting; }
wait_count() {          # kind count
    local k=$1 want=$2 i=0 f n
    while :; do
        n=0; for f in "$DIBS_LOCK_DIR"/$k.*; do [ -e "$f" ] && n=$((n+1)); done
        [ "$n" -ge "$want" ] && return 0
        i=$((i+1)); [ "$i" -gt 20000 ] && return 1
    done
}
held()   { wait_count holder "${1:-1}"; }
queued() { wait_count waiting "${1:-1}"; }
gone()   { local i=0 f n; while :; do
             n=0; for f in "$DIBS_LOCK_DIR"/holder.*; do [ -e "$f" ] && n=$((n+1)); done
             [ "$n" -eq 0 ] && return 0
             i=$((i+1)); [ "$i" -gt 20000 ] && return 1
           done; }
pidof_()  { $T --status | awk -v l=" $1 " 'index($0,l){for(i=1;i<=NF;i++) if($i=="pid") print $(i+1)}' | head -1; }

echo "exclusion"
check "idle to start" "$($T --status | grep -c 'dibs: idle')" "1"
fifo A; $T --bench --label bench-A "$(hold A)" >/dev/null 2>&1 &
BENCH=$!; held
check "a benchmark reads as busy" "$($T --status | grep -c 'BUSY, benchmark')" "1"
$T --wait 1 --label build-B 'echo no' >/dev/null 2>&1
check "it excludes shared users" "$?" "75"
$T --bench --wait 1 --label bench-C 'echo no' >/dev/null 2>&1
check "it excludes other benchmarks" "$?" "75"
free A; wait $BENCH
check "and frees on normal exit" "$(holders)" "0"

echo "sharing and fairness"
fifo S1; fifo S2
$T --label build-1 "$(hold S1)" >/dev/null 2>&1 & B1=$!; held
$T --label build-2 "$(hold S2)" >/dev/null 2>&1 & B2=$!; held 2
check "shared users run together" "$(holders)" "2"
free S1; free S2; wait $B1 $B2
fifo L; $T --label build-long "$(hold L)" >/dev/null 2>&1 & LONG=$!; held
$T --bench --label bench-q 'echo ran' > "$S/q" 2>/dev/null & Q=$!; queued
check "a benchmark queues with a position" "$($T --status | grep -c 'queued 1 of 1: bench')" "1"
$T --wait 1 --label build-late 'echo late' >/dev/null 2>&1
check "and gates later shared users" "$?" "75"
free L; wait $LONG $Q
check "then runs" "$(grep -c ran "$S/q")" "1"

echo "it says so before it waits"
fifo QN; $T --bench --label qn-holder "$(hold QN)" >/dev/null 2>&1 & QN=$!; held
out=$($T --wait 1 --label queued-notice 'echo nope' 2>&1)
check "a queued caller is told at once, not after the wait" \
  "$(grep -c 'queued and has not started' <<<"$out")" "1"
check "and told to background it" "$(grep -c 'run_in_background' <<<"$out")" "1"
free QN; wait $QN

echo "surviving abuse"
fifo D; $T --bench --label doomed "$(hold D)" >/dev/null 2>&1 & DOOMED=$!; held
pkill -9 -P $DOOMED 2>/dev/null; kill -9 $DOOMED 2>/dev/null; wait $DOOMED 2>/dev/null
# The record outlives a SIGKILL because no trap can run; the lock does not, because it is
# an open descriptor. So ask the question that matters: can the next job get in?
$T --bench --wait 5 --label after-kill 'echo recovered' > "$S/k" 2>/dev/null
check "the next benchmark gets the lock" "$(grep -c recovered "$S/k")" "1"
check "and status prunes the dead record" "$($T --status | grep -c 'dibs: idle')" "1"
fifo M; $T --bench --max 2 --label runaway "$(hold M)" >/dev/null 2>&1
check "--max kills an overrun" "$?" "124"
check "an overrun is not recorded as a duration" "$(grep -c runaway "$DIBS_HISTORY")" "0"

echo "estimating from the closest thing it has"
# Two thirds of the labels ever recorded on the real machine appear exactly once, because
# agents name the run rather than the kind of work. What that agent's other jobs took is the
# next best answer, and it beats the median of every job on the machine by a long way.
printf 'bench\tsome-run\t300\tAgent One\n' > "$DIBS_HISTORY"
printf 'bench\tanother-run\t300\tAgent One\n' >> "$DIBS_HISTORY"
printf 'bench\tunrelated\t5\tAgent Two\n' >> "$DIBS_HISTORY"
printf 'bench\tunrelated\t5\tAgent Two\n' >> "$DIBS_HISTORY"
printf 'bench\t%s\t%s\tnovel-run\tAgent One\tthe new one\n' "$$" "$(date +%s)" \
  > "$DIBS_LOCK_DIR/holder.$$"
check "it reaches for the agent before the machine" "$($T --status | grep -c "this agent's other bench jobs: usually 5m00s over 2")" "1"
check "and the json names that scope" "$($T --status --json | grep -c '"est_scope":"agent"')" "1"
# With a history of its own, that wins.
printf 'bench\tnovel-run\t60\tAgent One\n' >> "$DIBS_HISTORY"
printf 'bench\tnovel-run\t60\tAgent One\n' >> "$DIBS_HISTORY"
check "its own history wins when it has one" "$($T --status | grep -c 'usually 1m00s over 2 runs')" "1"
# An agent nobody has seen falls the rest of the way to the mode.
printf 'bench\t%s\t%s\tnever-run\tAgent Three\tthe new one\n' "$$" "$(date +%s)" \
  > "$DIBS_LOCK_DIR/holder.$$"
check "a label and an agent both unseen fall through to the mode" \
  "$($T --status | grep -c 'every bench job on the machine:')" "1"
rm -f "$DIBS_LOCK_DIR/holder.$$" "$DIBS_LOCK_DIR/cpu.$$" "$DIBS_HISTORY"

echo "going around a queued benchmark"
# A quick shared job behind a queued bench would otherwise wait for the whole bench. Every
# case here is fabricated from history and fifos, so none of it depends on timing.
printf 'shared\tquickie\t1\nshared\tquickie\t1\nshared\tquickie\t1\n' >> "$DIBS_HISTORY"
printf 'shared\tslowpoke\t120\nshared\tslowpoke\t120\nshared\tslowpoke\t120\n' >> "$DIBS_HISTORY"
fifo AN; fifo BL; fifo BY
$T --label anchor "$(hold AN)" >/dev/null 2>&1 & AND=$!; held
$T --bench --label blocked "$(hold BL)" >/dev/null 2>&1 & BLD=$!; queued

DIBS_PATIENCE=600 DIBS_QUICK=5 $T --label quickie "$(hold BY)" >/dev/null 2>&1 & BYD=$!
check "a quick job goes around it" "$(held 2 && holders)" "2"
check "and the bench is still queued, not passed over" "$($T --status | grep -c 'queued 1 of 1: bench')" "1"
check "the log says it went around" "$($T --log 20 | grep -c bypassed)" "1"
free BY; wait $BYD

# The same job, once the bench has been waiting longer than it will put up with.
DIBS_PATIENCE=0 DIBS_QUICK=5 $T --label quickie "$(hold BY)" >/dev/null 2>&1 & BYD=$!
check "past the bench's patience it waits its turn" "$(queued 2 && waiters)" "2"
free BY 2>/dev/null

# A job whose own history says it is not quick, and one with no history at all.
fifo SL
DIBS_PATIENCE=600 DIBS_QUICK=5 $T --label slowpoke "$(hold SL)" >/dev/null 2>&1 & SLD=$!
check "a job too slow to qualify waits" "$(queued 3 && waiters)" "3"
fifo NH
DIBS_PATIENCE=600 DIBS_QUICK=5 $T --label never-seen "$(hold NH)" >/dev/null 2>&1 & NHD=$!
check "and so does one with no history of its own" "$(queued 4 && waiters)" "4"

free AN; wait $AND
free BL; wait $BLD 2>/dev/null
free BY 2>/dev/null; free SL 2>/dev/null; free NH 2>/dev/null
wait $BYD $SLD $NHD 2>/dev/null
gone

echo "rescue paths"
fifo P; $T --bench --label blocker "$(hold P)" >/dev/null 2>&1 & P1=$!; held
check "--peek ignores the lock" "$($T --peek 'echo peeked' 2>/dev/null)" "peeked"
check "and does not register" "$($T --status | grep -c peek)" "0"
vpid=$(pidof_ blocker)
# Several agents read the same --status, so a pid copied out of it belongs to whoever happens
# to be holding the machine. Stopping someone else's measurement has to be deliberate.
out=$(CLAUDE_CODE_HOST_SESSION_ID=local_killer $T --kill "$vpid" 2>&1); rc=$?
check "--kill refuses a job that is not yours" "$rc" "2"
check "and names who it belongs to" "$(grep -c 'belongs to' <<<"$out")" "1"
check "and the job is still running" "$(holders)" "1"
check "--anyone is how you mean it" \
  "$(CLAUDE_CODE_HOST_SESSION_ID=local_killer $T --kill "$vpid" --anyone 2>&1 | grep -c 'It belonged to')" "1"
gone
check "--kill stops a holder" "$(holders)" "0"
wait $P1 2>/dev/null
check "--kill refuses an unknown pid" "$($T --kill 999999 >/dev/null 2>&1; echo $?)" "1"

echo "what it reports"
printf 'bench\teta\t600\nbench\teta\t660\nbench\teta\t620\n' >> "$DIBS_HISTORY"
fifo E; $T --bench --label eta "$(hold E)" >/dev/null 2>&1 & E1=$!; held
$T --bench --label eta --wait 30 'echo q1' >/dev/null 2>&1 & Q1=$!; queued
out=$($T --status)
check "median of 600,620,660 is 10m20s" "$(grep -c 'usually 10m20s over 3 runs' <<<"$out")" "1"
check "the queue gets an ETA" "$(grep -c 'until it starts' <<<"$out")" "1"
free E; wait $E1 $Q1
fifo N; $T --bench --label unseen "$(hold N)" >/dev/null 2>&1 & N1=$!; held
check "an unfamiliar label says the number is not its own" \
  "$($T --status | grep -c 'nothing on this one')" "1"
free N; wait $N1
mv "$DIBS_HISTORY" "$DIBS_HISTORY.aside"
fifo Z; $T --bench --label blank "$(hold Z)" >/dev/null 2>&1 & Z1=$!; held
$T --wait 30 --label waiter 'echo x' >/dev/null 2>&1 & W=$!; queued
out=$($T --status)
check "with no history it invents no duration" "$(grep -c 'no history for this one yet' <<<"$out")" "1"
check "and no ETA for the queue" "$(grep -c 'until it starts' <<<"$out")" "0"
check "but the waiter still has a position" "$(grep -c 'queued 1 of 1' <<<"$out")" "1"
free Z; wait $Z1 $W; mv "$DIBS_HISTORY.aside" "$DIBS_HISTORY"

echo "spotting a job that is stuck"
# A threshold of -1 flags anything the rate is willing to call idle, whatever its age, and an
# hour flags nothing: the age guard and the CPU reading are separate claims and are checked
# as such, rather than one of them passing because the other happened to be true.
fifo W; $T --bench --label wedged "$(hold W)" >/dev/null 2>&1 & WD=$!; held
# The idle signal is a rate, so it needs two looks to have anything to compare: the first one
# leaves the reading behind and says nothing. Asserting on a single look passes only when the
# fixture's own shells happened to burn no whole tick before it, which is a coin flip.
DIBS_IDLE_AFTER=-1 $T --status >/dev/null
# Which of the two idle sentences it gets still depends on that tick, and is not what is under
# test here. That it is flagged at all is. The other branch's wording is pinned by the burner.
check "a job that never starts working is flagged" \
  "$(DIBS_IDLE_AFTER=-1 $T --status | grep -c 'IDLE:')" "1"
check "and it is told how to stop it" \
  "$(DIBS_IDLE_AFTER=-1 $T --status | grep -c 'dibs --kill')" "1"
check "one younger than the threshold is left alone" \
  "$(DIBS_IDLE_AFTER=3600 $T --status | grep -c IDLE)" "0"
free W; wait $WD

# Burns a known amount of CPU in children it then reaps, and the same amount on any machine:
# a fixture sized in loop iterations is only a fixture on the laptop it was written on.
# Burns CPU time rather than wall time. `timeout 1.2` bounds the clock, and on a loaded
# machine 1.2s of clock buys a fraction of that in CPU, so the assertion below measured how
# busy the laptop was instead of whether reaped children are counted.
burner() { echo "for i in 1 2 3; do python3 -c 'import time
s = time.process_time()
while time.process_time() - s < 1.2: pass'; done
           printf 'x\n' > $S/f-$1
           $(hold "$2")"; }
fifo B; fifo BD; fifo BH
$T --bench --label busy "$(burner BD B)" >/dev/null 2>&1 & BZ=$!; held
$T --label behind "$(hold BH)" >/dev/null 2>&1 & BHD=$!; queued
sync_ BD
# The burner has stopped by here, so what separates these two lines is only that the second
# has something to compare against. A cumulative count could not tell them apart at all.
check "a job that has worked is not flagged on first sight" \
  "$(DIBS_IDLE_AFTER=-1 $T --status | grep -c IDLE)" "0"
check "one that worked and then stopped is flagged on the next look" \
  "$(DIBS_IDLE_AFTER=-1 $T --status | grep -c 'none of it in the last')" "1"
# A supervisor owns almost no CPU itself: its work was done by children it has already
# reaped, and counting only the living is what made a busy sweep read as idle.
cpu_of() { DIBS_NO_CHILDREN=${1:-0} DIBS_IDLE_AFTER=-1 $T --status |
           sed -n 's/.*IDLE: \([0-9]*\)s of CPU.*/\1/p'; }
check "the work its reaped children did is counted" "$([ "$(cpu_of)" -ge 3 ] && echo yes || echo no)" "yes"
# Which walk runs is a property of the --status call, not of the job, so one burner proves both.
# The fallback is unreachable on an ordinary kernel and is only ever exercised here.
check "and the fallback walk counts the same" "$(cpu_of 1)" "$(cpu_of)"
free B; wait $BZ

# The queued job has now been waiting as long as the burner ran, which is the whole point:
# what it shows as a holder is what it has been running, not what it has been alive.
until_ '$T --status | grep -qE "^  shared  behind"'
check "a holder's clock starts when it acquires, not when it arrived" \
  "$($T --status | grep -cE 'behind +[0-2]s')" "1"
free BH; wait $BHD

echo "peeks are supposed to be free"
export DIBS_PEEK_WARN=1
check "a cheap peek says nothing" "$($T --peek 'echo fine' 2>&1)" "fine"
out=$(CLAUDE_CODE_HOST_SESSION_ID=local_peeker $T --peek "timeout 1.5 python3 -c 'while True: pass'" 2>&1)
check "a costly one warns about the lock it skipped" "$(grep -c 'ran with no lock' <<<"$out")" "1"
check "and is recorded as peek-slow" "$(grep -c peek-slow "$DIBS_LOG")" "1"
check "and the peek-slow line names who did it" \
  "$($T --log 5 | grep peek-slow | grep -c 'session peeker')" "1"
unset DIBS_PEEK_WARN

echo "the log"
check "arrivals are logged" "$(grep -c 'arrived' "$DIBS_LOG")" "$(grep -c 'arrived' "$DIBS_LOG")"
check "outcomes are logged" "$([ "$(grep -c finished "$DIBS_LOG")" -ge 3 ] && echo yes || echo no)" "yes"
check "a kill is logged with its target" "$(grep -c 'killed.*blocker' "$DIBS_LOG")" "1"
check "a torn-down job is logged too" "$([ "$(grep -c aborted "$DIBS_LOG")" -ge 1 ] && echo yes || echo no)" "yes"
check "--log renders" "$($T --log 5 | head -1 | grep -c WHEN)" "1"

echo "watching"
check "an interval under the floor is refused" "$($T --watch 1 >/dev/null 2>&1; echo $?)" "2"
check "and it takes no command" "$($T --watch 5 'echo hi' >/dev/null 2>&1; echo $?)" "2"
out=$(timeout 2.5 $T --watch 2 2>/dev/null)
check "it redraws on the interval" "$(grep -c 'ctrl-c to stop' <<<"$out")" "2"
check "piped, it leaves the escape codes out" "$(grep -c $'\033' <<<"$out")" "0"
check "and it takes no lock" "$(holders)" "0"

echo "who ran it"
# Agents are told apart by their session, and a session with no title on disk still has an
# id. The point of carrying it is that the user can go and ask that agent what it was doing.
fifo A1
CLAUDE_CODE_HOST_SESSION_ID=local_deadbeef $T --bench --label owned "$(hold A1)" >/dev/null 2>&1 & A1D=$!; held
check "a holder says whose it is" "$($T --status | grep -c 'from session deadbeef')" "1"
fifo A2
CLAUDE_CODE_HOST_SESSION_ID=local_cafe $T --label behind-it "$(hold A2)" >/dev/null 2>&1 & A2D=$!; queued
check "and so does one still in the queue" "$($T --status | grep -c 'from session cafe')" "1"
check "the queue shows what each will run" "$($T --status | grep -c 'read -r _ <')" "2"
free A1; wait $A1D
free A2; wait $A2D
check "the log says who ran what" "$($T --log 20 | grep -c 'session deadbeef')" "2"
env -u CLAUDE_CODE_HOST_SESSION_ID -u CLAUDE_CODE_SESSION_ID $T --label byhand 'echo hi' >/dev/null 2>&1
check "a shell that is no agent says so instead" "$($T --log 4 | grep -c 'at a shell')" "2"

echo "queued shared jobs are not standing in line behind each other"
# The shared lock admits all of them at once, so the queue only advances at a benchmark.
# Adding their durations up told the third shared job it was waiting out the first two.
printf 'bench\tblocker\t100\nbench\tblocker\t100\nbench\tblocker\t100\n' > "$DIBS_HISTORY"
printf 'shared\tbuild-a\t60\nshared\tbuild-a\t60\nshared\tbuild-a\t60\n' >> "$DIBS_HISTORY"
printf 'shared\tbuild-b\t50\nshared\tbuild-b\t50\nshared\tbuild-b\t50\n' >> "$DIBS_HISTORY"
printf 'bench\tsweep\t200\nbench\tsweep\t200\nbench\tsweep\t200\n' >> "$DIBS_HISTORY"
NOWS=$(date +%s)
# Real pids, because prune drops any record whose process is not in /proc. Invented numbers
# pass or fail depending on what else the machine happens to be running at the time.
fifo Q
for q in 1 2 3; do bash -c "read -r _ < $S/f-Q" & eval "QP$q=\$!"; done
printf 'bench\t%s\t%s\tblocker\tan agent\tthe holder\n' "$$" "$NOWS" > "$DIBS_LOCK_DIR/holder.$$"
printf 'shared\t%s\t%s\tbuild-a\tan agent\tfirst build\n'  "$QP1" "$NOWS" > "$DIBS_LOCK_DIR/waiting.$QP1"
printf 'shared\t%s\t%s\tbuild-b\tan agent\tsecond build\n' "$QP2" "$((NOWS+1))" > "$DIBS_LOCK_DIR/waiting.$QP2"
printf 'bench\t%s\t%s\tsweep\tan agent\tthe sweep\n'       "$QP3" "$((NOWS+2))" > "$DIBS_LOCK_DIR/waiting.$QP3"
# Asserted as relations between the three ETAs rather than as formatted durations. The
# holder's remaining time is measured from now, so a second ticking over between writing the
# records and reading them turns 1m40s into 1m39s and a literal string comparison fails once
# in every few runs for a reason that has nothing to do with what is being tested.
ETAS=$($T --status --json | jq -c '[.queue[].eta]')
# Both shared jobs wait for the holder and for nothing else, so they start at the same moment.
check "the second shared job does not wait out the first" \
  "$(jq -r '.[0] == .[1]' <<<"$ETAS")" "true"
# The benchmark waits for the longest of them, not for their sum: max(60,50) after them, not 110.
check "a benchmark behind them waits for the longest, not the total" \
  "$(jq -r '.[2] - .[0] == 60' <<<"$ETAS")" "true"
check "and the queue is the three of them" "$(jq -r 'length' <<<"$ETAS")" "3"
check "the status display agrees with the json" \
  "$($T --status | grep -c 'until it starts')" "3"
free Q; wait "$QP1" "$QP2" "$QP3" 2>/dev/null
rm -f "$DIBS_LOCK_DIR"/waiting.* "$DIBS_LOCK_DIR/holder.$$" "$DIBS_LOCK_DIR/cpu.$$"

echo "an estimate says how sure it is"
# Half the labels on the real machine name a repo rather than a kind of work, so the same
# name covers a git status and a full build. A median is honest there and predicts nothing.
printf 'shared\tmixed\t0\nshared\tmixed\t0\nshared\tmixed\t0\nshared\tmixed\t200\nshared\tmixed\t240\n' > "$DIBS_HISTORY"
printf 'shared\t%s\t%s\tmixed\tan agent\tthe mixed job\n' "$$" "$(( $(date +%s) - 30 ))" \
  > "$DIBS_LOCK_DIR/holder.$$"
check "a label whose runs disagree says so" "$($T --status | grep -c 'anywhere from')" "1"
check "and does not dress it up as one number" "$($T --status | grep -c 'usually')" "0"
check "the json marks it too" "$($T --status --json | grep -c '"est_wide":true')" "1"
# Past the median it is in the tail, but the tail still has a shape. Surrendering there cost
# every waiter its ETA, because the labels that hold the machine longest median at zero.
check "past the median it still bounds the wait" "$($T --status | grep -c 'if it runs true to form')" "1"
check "and the json says which kind of answer that is" \
  "$($T --status --json | grep -c '"remaining_kind":"bound"')" "1"
rm -f "$DIBS_LOCK_DIR/holder.$$" "$DIBS_LOCK_DIR/cpu.$$" "$DIBS_HISTORY"

# One sample is a fact about one run, not a habit.
printf 'bench\tsolo\t120\n' > "$DIBS_HISTORY"
printf 'bench\t%s\t%s\tsolo\tan agent\tthe only run\n' "$$" "$(( $(date +%s) - 10 ))" \
  > "$DIBS_LOCK_DIR/holder.$$"
check "a single run does not claim a habit" "$($T --status | grep -c 'ran once, in 2m00s')" "1"
check "and does not say 1 runs" "$($T --status | grep -c '1 runs')" "0"
rm -f "$DIBS_LOCK_DIR/holder.$$" "$DIBS_LOCK_DIR/cpu.$$" "$DIBS_HISTORY"

echo "every user on a machine has to take the same lock"
# State under /run/user or /tmp is keyed by uid, so two people each took their own lock, each
# was told the machine was idle, and both benchmarked at once. Nothing reported it.
unset DIBS_LOCK_DIR
SHL=$S/shared-lock
check "with no shared directory the lock is keyed to a uid, and --check says so" \
  "$(DIBS_SHARED_LOCK_DIR=$SHL $T --check | grep -c 'keyed to this uid')" "1"
# A shared account is the other remedy and the better one here, so it has to be named: told
# only to make a group, someone with one account for everybody fixes a problem they do not have.
check "and names both remedies, not only the group" \
  "$(DIBS_SHARED_LOCK_DIR=$SHL $T --check | grep -c 'everyone this one account')" "1"
check "and tells you exactly how to fix it, naming the configured path" \
  "$(DIBS_SHARED_LOCK_DIR=$SHL $T --check | grep -c "install -d -m 2775 -g dibs $SHL")" "1"
mkdir -p "$SHL"
check "with one present it is used" \
  "$(DIBS_SHARED_LOCK_DIR=$SHL $T --check | grep -c "lock directory is shared: $SHL")" "1"
check "and a job actually takes its lock there" \
  "$(DIBS_SHARED_LOCK_DIR=$SHL $T --label shared-dir 'echo ran' >/dev/null 2>&1; [ -e "$SHL/rw" ] && echo yes)" "yes"
# A record has to be removable by whoever prunes it, not only by whoever wrote it, or one
# user's dead job wedges the queue for everyone else.
check "records are group-writable so another user can prune them" \
  "$(stat -c '%A' "$SHL"/* 2>/dev/null | head -1 | cut -c6)" "w"
rm -rf "$SHL"
export DIBS_LOCK_DIR="$DIBS_LOCK_DIR_SAVED"

echo "the status says where a job's output is going"
# --out reads a job by finding the file it redirected into, which is useless if nobody knows
# the feature exists. Naming the file in the status is how it gets discovered.
fifo R; : > "$S/redirected.log"
$T --label writes-output "bash -c '{ echo working; printf x > $S/f-Rready; $(hold R); } > $S/redirected.log 2>&1'" >/dev/null 2>&1 &
until [ -s "$S/f-Rready" ] 2>/dev/null; do :; done
# Matched on the line the status adds, not on the path: the path also appears in the command
# line, which is the job's own text and not evidence of anything.
check "the status names the file" "$($T --status | grep -c 'writing .*redirected.log')" "1"
check "and how to read it" "$($T --status | grep -c 'dibs --out')" "1"
check "the json carries it too" "$($T --status --json | grep -c '"output":')" "1"
free R; wait 2>/dev/null

# A job that redirected nowhere has nothing to offer, and a line saying so on every tick of a
# watch is noise.
fifo T2
$T --label no-output "printf x > $S/f-T2ready; $(hold T2)" >/dev/null 2>&1 &
until [ -s "$S/f-T2ready" ] 2>/dev/null; do :; done
check "a job writing to no file says nothing" "$($T --status | grep -c 'dibs --out')" "0"
free T2; wait 2>/dev/null

echo "checking a machine before trusting it"
check "--check reports the tools nothing works without" "$($T --check | grep -c 'flock and timeout')" "1"
check "it proves the bootstrap parsed by having run at all" "$($T --check | grep -c 'parsed the bootstrap')" "1"
check "it names the cpu" "$($T --check | grep -c '    cpu   ')" "1"
# rocm-smi is installed on machines with no AMD GPU, prints a driver error, and exits 0.
# Presence of a tool and its exit status both say nothing about presence of hardware.
check "a machine with nothing to run on is not called ready" \
  "$(PATH=/nonexistent:$PATH $T --check 2>/dev/null | grep -c 'ready\.')" "0"
check "--check takes only a host" "$($T --check a b 2>&1 >/dev/null | grep -c 'only a host')" "1"

echo "reading what a running job is writing"
# A job's stdout goes back down the channel to whoever started it and is kept nowhere. But
# agents redirect into a file, and a redirect is an open descriptor the kernel will name, so
# the output is not lost, only somewhere nobody thought to look. The redirect an agent writes
# is always below the shells the wrapper puts in the way, hence the walk rather than a look
# at the holder alone.
fifo O; : > "$S/job.log"
$T --label writes-a-log "bash -c '{ echo first line; echo second line; printf x > $S/f-Oready; $(hold O); } > $S/job.log 2>&1'" >/dev/null 2>&1 &
until [ -s "$S/f-Oready" ] 2>/dev/null; do :; done
check "it finds the file a nested redirect opened" "$($T --out | grep -c 'job.log')" "1"
check "and shows what is in it" "$($T --out | grep -c 'second line')" "1"
check "naming the job's own pid works too" \
  "$($T --out "$(ls "$DIBS_LOCK_DIR"/holder.* | sed 's/.*\.//')" | grep -c 'second line')" "1"
free O; wait 2>/dev/null

# The wrapper's own stdout is inherited by everything in the tree. Reporting it would show
# every job the caller's terminal instead of the job's own output.
fifo P
$T --label no-log "printf x > $S/f-Pready; $(hold P)" > "$S/caller.txt" 2>&1 &
until [ -s "$S/f-Pready" ] 2>/dev/null; do :; done
check "a job that does not redirect says so" "$($T --out | grep -c 'copy on disk to show')" "1"
check "and does not offer the caller's own stdout as output" "$($T --out | grep -c 'caller.txt')" "0"
free P; wait 2>/dev/null
check "with nothing running it says so" "$($T --out | grep -c 'Nothing is running')" "1"
check "an unknown pid is an error, not an empty answer" "$($T --out 999999 2>&1 >/dev/null | grep -c 'Nothing holding')" "1"

echo "an overrun is measured against the same job, not its mode"
# Fabricated rather than run: the point is a holder that is old relative to a median built
# from entirely different work, which no fixture can produce quickly.
printf 'bench\tother-work\t20\nbench\tother-work\t20\nbench\tother-work\t20\n' > "$DIBS_HISTORY"
printf 'bench\t%s\t%s\tlong-one\tsome agent\tthe long job\n' "$$" "$(( $(date +%s) - 600 ))" \
  > "$DIBS_LOCK_DIR/holder.$$"
check "a mode-wide median does not accuse it" "$($T --status | grep -c 'STUCK')" "0"
check "and it says the history is not about this job" "$($T --status | grep -c 'nothing on this one')" "1"
check "nor does the json claim an overrun" "$($T --status --json | grep -c overrun)" "0"
# The same job, with a history of its own, is a real comparison.
printf 'bench\tlong-one\t20\nbench\tlong-one\t20\nbench\tlong-one\t20\n' >> "$DIBS_HISTORY"
check "its own median does" "$($T --status | grep -c 'STUCK')" "1"
check "and the json agrees" "$($T --status --json | grep -c '"overrun":true')" "1"
# Durations are whole seconds, so anything quicker than one records as zero, and a median of
# zero made every run of a quick job "3x its usual 0s" in --json while --status said nothing.
printf 'bench\tquick\t0\nbench\tquick\t0\nbench\tquick\t0\n' > "$DIBS_HISTORY"
printf 'bench\t%s\t%s\tquick\tsome agent\tthe quick job\n' "$$" "$(( $(date +%s) - 600 ))" \
  > "$DIBS_LOCK_DIR/holder.$$"
check "a median of zero accuses nobody" "$($T --status | grep -c STUCK)" "0"
check "and the json agrees with the display" "$($T --status --json | grep -c overrun)" "0"
check "it says what zero seconds means" "$($T --status | grep -c 'under a second')" "1"

# Past its usual duration, how much longer it has is not knowable, and zero reads to everyone
# behind it as "any moment now".
printf 'bench\tsteady\t100\nbench\tsteady\t100\nbench\tsteady\t100\n' > "$DIBS_HISTORY"
printf 'bench\t%s\t%s\tsteady\tsome agent\tthe steady job\n' "$$" "$(( $(date +%s) - 150 ))" \
  > "$DIBS_LOCK_DIR/holder.$$"
printf 'shared\t%s\t%s\tbehind-it\tsome agent\tthe waiting job\n' "$PPID" "$(date +%s)" \
  > "$DIBS_LOCK_DIR/waiting.$PPID"
check "an overdue job does not claim a remainder" "$($T --status | grep -c 'left\]')" "0"
check "it says it is past its usual instead" "$($T --status | grep -c 'longer than it has ever taken')" "1"
check "nor does the json invent one" "$($T --status --json | grep -c remaining)" "0"
check "and nothing behind it is promised a start" "$($T --status | grep -c 'until it starts')" "0"
check "which the json leaves out too" "$($T --status --json | grep -c '"eta"')" "0"
rm -f "$DIBS_LOCK_DIR/waiting.$PPID"

rm -f "$DIBS_LOCK_DIR/holder.$$" "$DIBS_LOCK_DIR/cpu.$$" "$DIBS_HISTORY"

echo "machine-readable output"
# A reader must never have to parse the human display, so the shapes are pinned here.
fifo J
CLAUDE_CODE_HOST_SESSION_ID=local_jsonner $T --bench --label "j-holder" \
  "$(hold J)" >/dev/null 2>&1 & JD=$!; held
jq_() { $T --status --json | python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
check "it parses" "$(jq_ "'ok'")" "ok"
check "the state is named" "$(jq_ "d['state']")" "bench"
check "the holder is there" "$(jq_ "d['holders'][0]['label']")" "j-holder"
check "with its agent" "$(jq_ "d['holders'][0]['agent']")" "session jsonner"
check "and no colour ever" "$($T --status --json | grep -c $'\033')" "0"
# A command with quotes and a backslash in it is the case that breaks hand-rolled JSON.
fifo J2
$T --label 'j-queued' "echo \"a\\b\" > /dev/null; $(hold J2)" >/dev/null 2>&1 & J2D=$!; queued
check "a queued entry survives quoting" "$(jq_ "d['queue'][0]['label']")" "j-queued"
check "and its command round-trips" "$(jq_ "'\\\\' in d['queue'][0]['cmd']")" "True"
free J; wait $JD
free J2; wait $J2D 2>/dev/null

echo "transfers are jobs too"
# The transfer itself needs a real channel and lives in the live suite. What can be pinned
# here is that a malformed one is refused before anything is reached for.
check "--sync wants the machine's side marked" "$($T --sync ./a ./b >/dev/null 2>&1; echo $?)" "2"
check "and it wants two paths" "$($T --sync :~/x >/dev/null 2>&1; echo $?)" "2"
check "--rsh is not for hands" "$($T --rsh >/dev/null 2>&1; echo $?)" "2"
check "and on the machine itself a copy is just a copy" \
  "$($T --sync ./a :~/b 2>&1 | grep -c 'You are on it')" "1"

echo "unreachable machine"
out=$(DIBS_LOCAL=0 DIBS_HOSTNAME=nowhere DIBS_HOST=nowhere.invalid \
      DIBS_CONNECT_TIMEOUT=2 $T --status 2>&1); rc=$?
check "fails fast with exit 69" "$rc" "69"
check "and tells the agent not to loop" "$(grep -c 'Do not retry in a loop' <<<"$out")" "1"
# ssh's own answer, not a guess. A machine reached over the LAN is not a tailnet peer, and
# reporting one as "off or asleep" because tailscale has not heard of it is a true statement
# about tailscale and a false diagnosis: it sends someone to look at a machine that is fine.
check "it says what ssh actually complained about" \
  "$(grep -c 'does not resolve from here' <<<"$out")" "1"
check "and does not blame the tailnet for a machine that is not on it" \
  "$(grep -ci 'not on the tailnet' <<<"$out")" "0"

echo "scratch"
# --check tells whoever is fixing a broken machine to set DIBS_SCRATCH. It has to be the
# variable the code actually reads, or that advice sends them somewhere nothing happens.
check "DIBS_SCRATCH is what the job gets" \
  "$(DIBS_SCRATCH=$S/scr $T --label scr 'echo $DIBS_SCRATCH' 2>/dev/null | tail -1)" "$S/scr"
check "and DIBS_SCRATCH still works" \
  "$(DIBS_SCRATCH=$S/scr2 $T --label scr 'echo $DIBS_SCRATCH' 2>/dev/null | tail -1)" "$S/scr2"

echo "inventory"
export DIBS_MACHINES=$S/machines.toml
cat > "$DIBS_MACHINES" <<'TOML'
default = "desk"

[machine.desk]
ssh      = "dibs@desk"
hostname = "desk"

  [[machine.desk.device]]
  kind = "gpu"
  name = "a device, not the machine"

[machine.lap]
ssh      = "lap"
hostname = "somewhere-else"
measure  = false
TOML
check "lists every machine" "$($T --machines | wc -l)" "2"
check "marks the default" "$($T --machines | awk '$1 == "*" {print $2}')" "desk"
check "says which one refuses measurements" "$($T --machines | grep -c 'no measurements')" "1"
# A device table's own keys must not answer for the machine's, or a card's name becomes the
# machine's ssh alias.
check "a device key does not answer for the machine" \
  "$(DIBS_LOCAL=0 DIBS_HOST= DIBS_CONNECT_TIMEOUT=2 $T --on desk --status 2>&1 |
     grep -c "cannot reach 'dibs@desk'")" "1"
out=$(DIBS_HOST= $T --on nope --status 2>&1); rc=$?
check "an unknown machine is refused" "$rc" "2"
check "and the known ones are named" "$(grep -c 'desk' <<<"$out")" "1"
# The point of the flag: a machine that cannot produce a trustworthy number must not be
# able to produce one at all, rather than everyone remembering not to ask it.
out=$(DIBS_HOST= $T --on lap --bench true 2>&1); rc=$?
check "a benchmark is refused where measure is false" "$rc" "2"
check "and it says why" "$(grep -c 'measure = false' <<<"$out")" "1"
# Writing an inventory must not move work that was already going somewhere.
check "DIBS_HOST outranks the file's default" \
  "$(DIBS_HOST=pinned.invalid DIBS_HOSTNAME=pinned DIBS_CONNECT_TIMEOUT=2 DIBS_LOCAL=0 \
     $T --status 2>&1 | grep -c 'desk')" "0"

# The default is where an unqualified benchmark goes. Recording a laptop must not take it
# from a machine that can actually measure, or every --bench starts failing.
cat > "$DIBS_MACHINES" <<'TOML'
default = "desk"

[machine.desk]
ssh      = "dibs@desk"
hostname = "desk"
TOML
$T --check laptop --write >/dev/null 2>&1
# --check was the one command that dialled its argument literally instead of resolving it,
# so `--check <name>` reached a different host string than every other command uses: one that
# may not resolve and that nothing has a host key for. The command whose job is to say whether
# a machine is usable was the one that could not reach it.
# DIBS_LOCAL=0 because the whole point is which host string reaches the far side, and the rest
# of this suite never leaves the machine. Neither name resolves, so both fail: what is asserted
# is which one it tried.
check "a known name is resolved, not dialled literally" \
  "$(DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --check desk 2>&1 | grep -c "cannot reach 'dibs@desk'")" "1"
check "and a name nobody has recorded is still taken literally, so it can onboard one" \
  "$(DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --check brand-new-host 2>&1 | grep -c "cannot reach 'brand-new-host'")" "1"
check "recording a machine does not steal the default" \
  "$(awk -F'"' '/^default/ {print $2; exit}' "$DIBS_MACHINES")" "desk"
check "and the new machine is there too" "$($T --machines | wc -l)" "2"
# An integrated GPU is on the root complex with no link of its own, and reports width 0
# against a max of 255. Answering for it divided by zero and took the whole entry with it,
# so a laptop could not be recorded at all.
check "a device with no pcie link of its own does not break the write" \
  "$($T --check laptop --write 2>&1 | grep -c 'reported nothing to record')" "0"

# Renaming a machine leaves an entry that can never answer, and until there was a command for
# it the only fix was editing the file by hand.
cat > "$DIBS_MACHINES" <<'TOML'
default = "old"

[machine.old]
ssh      = "old"
hostname = "old"
measure  = false

  [[machine.old.device]]
  kind = "gpu"
  name = "a device whose parent is going away"

[machine.new]
ssh      = "new"
hostname = "new"
TOML
out=$($T --forget old 2>&1); rc=$?
check "forgetting a machine succeeds" "$rc" "0"
check "and it is gone" "$($T --machines | wc -l)" "1"
check "its device table goes with it" "$(grep -c 'parent is going away' "$DIBS_MACHINES")" "0"
# A default naming a machine that is gone sends every unpinned call nowhere.
check "the default is repointed" "$(awk -F'"' '/^default/ {print $2; exit}' "$DIBS_MACHINES")" "new"
check "forgetting one that is not there is refused" "$($T --forget nope >/dev/null 2>&1; echo $?)" "2"

echo "shared registry"
export DIBS_REGISTRY_CACHE=$S/registry.toml
cat > "$DIBS_REGISTRY_CACHE" <<'TOML'
default = "team-box"

[machine.team-box]
ssh      = "dibs@team-box"
hostname = "team-box"

[machine.contested]
ssh      = "dibs@from-registry"
hostname = "from-registry"
TOML
cat > "$DIBS_MACHINES" <<'TOML'
[machine.mine]
ssh      = "dibs@mine"
hostname = "mine"

[machine.contested]
ssh      = "dibs@from-mine"
hostname = "from-mine"
TOML
check "both layers are listed" "$($T --machines | wc -l)" "3"
check "a shared machine is usable without writing it out" "$($T --machines | grep -c 'team-box')" "1"
check "and it says which layer it came from" "$($T --machines | grep -c '\[shared\]')" "1"
# Two: the one only you have, and the shared one you overrode, which is yours now.
check "your own machines are marked as yours" "$($T --machines | grep -c '\[yours\]')" "2"
# Half an entry from each file would describe a machine that exists nowhere, so the personal
# entry has to win outright rather than key by key.
check "a personal entry overrides the shared one whole" \
  "$(DIBS_LOCAL=0 DIBS_HOST= DIBS_CONNECT_TIMEOUT=2 $T --on contested --status 2>&1 |
     grep -c 'dibs@from-mine')" "1"
check "the shared default is used when you have none" \
  "$($T --machines | awk '$1 == "*" {print $2}')" "team-box"
# The shared list is not yours to edit, and saying so beats appearing to succeed.
out=$($T --forget team-box 2>&1); rc=$?
check "a shared machine cannot be forgotten locally" "$rc" "2"
check "and it says why" "$(grep -c 'shared registry' <<<"$out")" "1"
check "your own can" "$($T --forget mine >/dev/null 2>&1; echo $?)" "0"
# A personal default has to beat the shared one, or where your work goes needs everyone to agree.
printf 'default = "mine2"\n\n[machine.mine2]\nssh = "dibs@mine2"\nhostname = "mine2"\n' > "$DIBS_MACHINES"
check "a personal default wins" "$($T --machines | awk '$1 == "*" {print $2}')" "mine2"
unset DIBS_REGISTRY_CACHE

echo "routing"
cat > "$DIBS_MACHINES" <<TOML
default = "one"

[machine.one]
ssh      = "one"
hostname = "$(hostname -s)"
workstation = true

[machine.two]
ssh      = "two"
hostname = "two"
TOML
check "picks a machine from the inventory" \
  "$($T --pick 2>/dev/null | grep -cE '^(one|two)$')" "1"
# Ranking has to be able to prefer another machine over the one doing the dispatching, or a
# build takes every thread on the machine its owner is trying to work on.
check "a machine someone works at is ranked behind an equal one" \
  "$(DIBS_SELF_PENALTY=10000 $T --pick 2>/dev/null)" "two"
check "-v says why" \
  "$(DIBS_SELF_PENALTY=10000 $T --pick -v 2>&1 >/dev/null | grep -c 'someone works here')" "1"
# A build ranked onto one machine and a benchmark pinned to another leaves the benchmark to
# compile inside its own exclusive lock, so holding the cache outranks being less busy.
check "the machine holding the cache wins anyway" \
  "$(DIBS_SELF_PENALTY=10000 $T --pick --prefer one 2>/dev/null)" "one"
check "and it says that is why" \
  "$(DIBS_SELF_PENALTY=10000 $T --pick -v --prefer one 2>&1 >/dev/null | grep -c 'holds the cache')" "1"
check "a preferred machine that cannot answer is not used" \
  "$(DIBS_SELF_PENALTY=10000 $T --pick --prefer nowhere 2>/dev/null)" "two"
check "--which names the machine without ranking" "$($T --which)" "one"
# The onboarding script sets DIBS_MACHINE to the host it is introducing you to. If dibs read
# that as an inventory entry, a new user's very first command would fail with an error about
# an inventory they do not have.
check "a name meant for the onboarding script is not read as a pin" \
  "$(DIBS_MACHINE=some-host-they-were-given $T --which)" "one"
check "DIBS_ON pins to an inventory machine" "$(DIBS_ON=two $T --which)" "two"
# Both fake machines here are this one, so they always report the same caches and the case
# that matters, a busy machine with the cache beating an idle one without it, cannot be built
# from them. What is checked here is that a real cargo target is reported and matched at all.
mkdir -p "$S/scr/target/faux" && : > "$S/scr/target/faux/.rustc_info.json"
mkdir -p "$S/scr/target/prepared-only"
check "a machine reports the repos it has actually built" \
  "$(DIBS_SCRATCH=$S/scr $T --status --json | grep -c '"caches":\["faux"\]')" "1"
check "a target directory nothing was built in is not a cache" \
  "$(DIBS_SCRATCH=$S/scr $T --status --json | grep -c 'prepared-only')" "0"

# A worktree is prepared from a clone under ~/prog, so a machine without one cannot run the
# job at all. Ranking it last is not enough: last still wins when it is the only machine that
# answered, and the job then queues for a machine that fails the moment it starts.
mkdir -p "$S/fakehome/prog/faux/.git"
check "a machine reports the repos it can prepare from" \
  "$(HOME=$S/fakehome $T --status --json | grep -c '"clones":\["faux"\]')" "1"
check "a machine with no clone of the repo is not chosen" \
  "$(HOME=$S/fakehome DIBS_SELF_PENALTY=0 $T --pick --repo absent 2>/dev/null; echo "exit=$?")" "exit=69"
check "-v says why it was dropped" \
  "$(HOME=$S/fakehome $T --pick --repo absent -v 2>&1 >/dev/null | grep -c 'no clone of absent')" "2"
check "a machine that does have the clone is still chosen" \
  "$(HOME=$S/fakehome DIBS_SELF_PENALTY=10000 $T --pick --repo faux 2>/dev/null)" "two"
# Affinity is a memo about where a cache is, and a cache is no use on a machine that cannot
# prepare the tree in the first place.
check "and affinity does not override a missing clone" \
  "$(HOME=$S/fakehome $T --pick --repo absent --prefer one 2>/dev/null; echo "exit=$?")" "exit=69"
# Two failures that need different fixes: nothing is up, versus everything is up and none of
# it can prepare this repo. Reporting the second as the first sends you to look at the network.
check "and says which of the two failures it was" \
  "$(HOME=$S/fakehome $T --pick --repo absent 2>&1 >/dev/null | grep -c 'none has a clone')" "1"
check "--repo picks a machine that reports it" \
  "$(HOME=$S/fakehome DIBS_SCRATCH=$S/scr $T --pick --repo faux 2>/dev/null | grep -cE '^(one|two)$')" "1"
# A first build decides where a repo's cache lives, and a machine that refuses benchmarks is
# one no benchmark can follow it to. Uncached, but cloned: a machine with no clone is out of
# the ranking entirely and would not be there to lose it.
mkdir -p "$S/fakehome/prog/never-built/.git"
cat > "$DIBS_MACHINES" <<TOML
default = "measures"

[machine.measures]
ssh      = "measures"
hostname = "$(hostname -s)"

[machine.refuses]
ssh      = "refuses"
hostname = "refuses"
measure  = false
TOML
check "an uncached repo goes to a machine that can measure it" \
  "$(HOME=$S/fakehome DIBS_SELF_PENALTY=100 $T --pick --repo never-built 2>/dev/null)" "measures"
# An inventory whose default cannot be reached, next to one that can. Without routing the job
# goes where it was told and fails; with it, the machine that answered is chosen. This is also
# the only assertion that a machine which did not answer is never picked.
cat > "$DIBS_MACHINES" <<TOML
default = "gone"

[machine.here]
ssh      = "here"
hostname = "$(hostname -s)"

[machine.gone]
ssh      = "nowhere.invalid"
hostname = "gone"
TOML
out=$(DIBS_HOST= DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --label r true 2>&1); rc=$?
check "without routing a job goes where it was told" "$rc" "69"
out=$(DIBS_HOST= DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --any --label r true 2>&1); rc=$?
check "with routing it goes to the machine that answered" "$rc" "0"
# A machine dropping out of the ranking silently degrades this to "whichever one answered",
# which looks exactly like a working ranking.
check "a machine that did not answer says so" \
  "$(DIBS_HOST= DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --pick -v 2>&1 >/dev/null |
     grep -c 'gone .*no answer')" "1"
# dibs-run prepares a worktree on one machine and then runs against it, so every step of a run
# has to land on the same machine. It picks once and pins; the wrapper must not pick again.
out=$(DIBS_HOST= DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 DIBS_FROM_RUN=1 DIBS_ROUTE=1 \
      $T --label r true 2>&1); rc=$?
check "a step of a run is never routed on its own" "$rc" "69"

echo "jobs that outlive their caller"
export DIBS_QUEUE=box@elsewhere DIBS_QUEUE_LOCAL=1 DIBS_JOBS_DIR=$S/jobs
fifo dj
# The whole point: the submitting session goes away and the work does not. So the submit has
# to return while the job is still going, which is what blocking it on a fifo proves.
id=$($T --detach --label outlives "read -r _ < $S/f-dj; echo released")
check "--detach returns a job id" \
  "$(printf '%s' "$id" | grep -cE '^[0-9]{8}-[0-9]{6}-[0-9]+$')" "1"
check "and the submit returned while the job is still running" \
  "$($T --jobs | awk -v i="$id" '$1 == i {print $3}')" "running"
free dj
until_ "[ -f $S/jobs/$id/status ]"
check "its exit status is kept for later" "$(cat "$S/jobs/$id/status")" "0"
check "and so is what it printed" "$($T --job "$id" | tail -1)" "released"
check "an unknown job is refused" "$($T --job no-such-job >/dev/null 2>&1; echo $?)" "2"
# An agent that realises the command was wrong has to be able to stop it, including while it
# is still waiting for a machine rather than running on one.
fifo dk
id2=$($T --detach --label cancelme "read -r _ < $S/f-dk; echo never")
check "a job can be stopped" "$($T --cancel "$id2" | grep -c stopped)" "1"
until_ "! kill -0 $(cat "$S/jobs/$id2/pid") 2>/dev/null"
check "and it really is gone" \
  "$(kill -0 "$(cat "$S/jobs/$id2/pid")" 2>/dev/null; echo $?)" "1"
check "cancelling one that already finished says so" \
  "$($T --cancel "$id" | grep -c 'already finished')" "1"
check "cancelling an unknown job is refused" "$($T --cancel nope >/dev/null 2>&1; echo $?)" "2"
# One account runs everyone's jobs on that machine, so the account cannot say whose a job is
# and stopping someone else's has to be as deliberate as it is on a benchmarking machine.
fifo dm
id3=$(CLAUDE_CODE_HOST_SESSION_ID=local_owner $T --detach --label theirs "read -r _ < $S/f-dm")
out=$(CLAUDE_CODE_HOST_SESSION_ID=local_other $T --cancel "$id3" 2>&1); rc=$?
check "another agent's job is not yours to cancel" "$rc" "2"
check "and it says whose it is" "$(grep -c 'belongs to' <<<"$out")" "1"
check "--jobs says who each belongs to" "$($T --jobs | grep -c 'session owner')" "1"
check "--anyone cancels it" \
  "$(CLAUDE_CODE_HOST_SESSION_ID=local_other $T --cancel "$id3" --anyone | grep -c stopped)" "1"
# Without somewhere that stays up there is nothing to detach onto, and saying so beats
# running it here and having it die with the session anyway.
check "with no queue it refuses rather than pretending" \
  "$(DIBS_QUEUE= $T --detach true >/dev/null 2>&1; echo $?)" "2"

# Both used to set the mode, so one silently erased the other and the order on the line
# decided which. Measured with no lock one way round, detached-in-name-only the other.
for order in "--bench --detach" "--detach --bench"; do
    out=$($T $order --label bd true 2>&1); rc=$?
    check "$order is refused rather than half-honoured" "$rc" "2"
    check "  and it says the lock is not taken" "$(grep -c 'does not take the lock' <<<"$out")" "1"
    check "  and it names the form that does" "$(grep -c "detach 'dibs --bench" <<<"$out")" "1"
done
check "no job was submitted by either" "$($T --jobs | grep -c ' bd ')" "0"
# The refusal names a form, and a named form nobody exercised is how a helpful message turns
# into a wrong one. The detached caller has to reach dibs and take the lock it was sent for.
fifo db
$T --detach --label inner "$T --bench --label inner-bench '$(hold db)'" >/dev/null
held
check "the form the refusal names does take the lock" \
  "$($T --status | awk '/BUSY, benchmark/{b=1} b && /inner-bench/{n++} END{print n+0}')" "1"
free db
gone
# The same silence for everything else that describes a run rather than the caller: it went
# to a queue that had no idea what to do with it, and the job ran as if it were never given.
for flag in "--device gpu:none" --new-series "--wait 5" "--max 60"; do
    check "$flag with --detach is refused" \
      "$($T --detach $flag true >/dev/null 2>&1; echo $?)" "2"
done
# Its counterpart: a detached job is not ranked across the pool, because --jobs reads one
# machine and a scattered job is one nobody can find again.
check "a detached job is not routed away from its queue" \
  "$(DIBS_ROUTE=1 DIBS_HOST= $T --detach --label routed true | grep -cE '^[0-9]{8}-')" "1"
unset DIBS_QUEUE DIBS_QUEUE_LOCAL DIBS_JOBS_DIR

# A compilation cache runs the compiler inside its own daemon, which is parented to init, so
# the work happens outside the job's process tree and the tree looks idle. Calling that stalled
# would have dibs tell people to kill healthy builds.
echo "a job that writes is a job that works"
fifo W
# Redirected in a child, the way dibs-run redirects every step. A holder's own fd 1 is the
# channel it was launched down and is excluded on purpose.
$T --label writes-out "sh -c 'read -r _ < $S/f-W' > $S/written" >/dev/null 2>&1 & W1=$!; held
until_ "[ -e $S/written ]"
check "a stalled tree with a fresh log is not called idle" \
  "$(DIBS_IDLE_AFTER=-1 $T --status --json | grep -c '\"idle_for\"')" "0"
# And the rule has to be able to say idle, or it says nothing at all.
check "an old log does not rescue it" \
  "$(DIBS_IDLE_AFTER=-1 DIBS_WROTE_WITHIN=-1 $T --status --json | grep -c '\"idle_for\"')" "1"
free W; wait $W1 2>/dev/null; gone

# A lock held with nothing to show for it stops the machine, and being told to go and run
# fuser yourself is asking for work at the moment you are least able to do it.
# Ownership cannot key on the title: it goes stale, two sessions can share one, and one that
# changes mid-run would make an agent a stranger to its own job.
echo "a job is owned by a session, not by a name"
fifo Q
CLAUDE_CODE_HOST_SESSION_ID=local_ident $T --label ident "$(hold Q)" >/dev/null 2>&1 & Q1=$!; held
check "the record carries the session" \
  "$(awk -F'\t' 'NR==1{print $6}' "$DIBS_LOCK_DIR"/holder.*)" "local_ident"
check "and the title beside it" \
  "$(awk -F'\t' 'NR==1{print $5}' "$DIBS_LOCK_DIR"/holder.*)" "session ident"
check "the command still lands in the last field" \
  "$(awk -F'\t' 'NR==1{print ($7 != "")}' "$DIBS_LOCK_DIR"/holder.*)" "1"
free Q; wait $Q1 2>/dev/null; gone

# A long compile hitting --max is ordinary, and the message is the whole of what makes it
# ordinary: without it, exit 124 reads as the job having gone wrong.
echo "overrunning says what to do about it"
out=$($T --max 1 --label overran 'python3 -c "
while True: pass"' 2>&1); rc=$?
check "an overrun exits 124" "$rc" "124"
check "it says it was stopped, not that it failed" "$(grep -c 'stopped after holding' <<<"$out")" "1"
check "and what running it again would do" "$(grep -c 'picks up from the crates' <<<"$out")" "1"
gone

echo "an orphaned lock names what holds it"
exec 8>"$DIBS_LOCK_DIR/rw"
flock -s 8
out=$($T --status 2>&1)
exec 8>&-
check "it is reported as an orphan" "$(grep -c 'LOCKED BY AN ORPHAN' <<<"$out")" "1"
check "and it says what is holding it" "$(grep -cE 'holding it:|reports holding it' <<<"$out")" "1"
check "back to idle once released" "$($T --status | grep -c 'dibs: idle')" "1"

echo "a job can see a toolchain the login shell would have set up"
if [ -d "$HOME/.cargo/bin" ]; then
    out=$($T --label path-cargo 'case ":$PATH:" in *":$HOME/.cargo/bin:"*) echo yes ;; *) echo no ;; esac' 2>/dev/null)
    check "cargo installed by rustup is on the path" "$out" "yes"
fi
out=$($T --label path-twice 'printf "%s\n" "$PATH" | tr ":" "\n" | sort | uniq -d | grep -c cargo' 2>/dev/null)
check "and is not added twice" "$out" "0"

echo "naming a device"
cat > "$DIBS_MACHINES" <<TOML
default = "rig"

[machine.rig]
ssh      = "rig"
hostname = "$(hostname -s)"

  [[machine.rig.device]]
  kind     = "gpu"
  alias    = "gpu:one"
  pci      = "0000:07:00.0"
  chip     = "10de:1f08"
  runtimes = ["cuda", "vulkan"]

  [[machine.rig.device]]
  kind     = "gpu"
  alias    = "gpu:twin.03"
  pci      = "0000:03:00.0"
  chip     = "1002:731f"
  runtimes = ["vulkan"]

  [[machine.rig.device]]
  kind     = "gpu"
  alias    = "gpu:twin.06"
  pci      = "0000:06:00.0"
  chip     = "1002:731f"
  runtimes = ["vulkan"]

  [[machine.rig.device]]
  kind     = "gpu"
  alias    = "gpu:lone"
  pci      = "0000:09:00.0"
  chip     = "8086:b080"
  runtimes = ["vulkan"]

[machine.other]
ssh      = "other"
hostname = "other"

  [[machine.other.device]]
  kind     = "gpu"
  alias    = "gpu:elsewhere"
  pci      = "0000:0a:00.0"
  chip     = "8086:c0de"
  runtimes = ["vulkan"]
TOML
# Repeatability is the whole point: two calls naming one alias have to reach one card, and a
# bus id is the only name for it that survives a reboot or another card being added.
# CUDA_VISIBLE_DEVICES takes an index or a GPU-<uuid>, never a bus id: handed one it does not
# error, it ignores the value and leaves every card visible. So the bus id is translated on the
# machine, and a machine that cannot answer for the card refuses the job rather than running it
# on whichever card is first and reporting it under the name that was asked for.
check "a card this machine cannot answer for is refused, not run unpinned" \
  "$($T --on rig --device gpu:one --label dev 'echo ran' 2>&1 >/dev/null | grep -c 'nothing here answers to it')" "1"
check "and nothing ran" \
  "$($T --on rig --device gpu:one --label dev 'echo ran' 2>/dev/null | grep -c ran)" "0"
# awk's exit jumps to END, so a flush() that printed and exited printed again on the way out.
# Two lines in a device selector is not a cosmetic fault, it is an unusable value.
check "a lookup answers once, not twice" \
  "$($T --on rig --device gpu:lone --label dev 'printf %s "$MESA_VK_DEVICE_SELECT"' 2>/dev/null | tail -1 | wc -l)" "0"
check "the job can see which device it was given" \
  "$($T --on rig --device gpu:lone --label dev 'printf %s "$DIBS_DEVICE"' 2>/dev/null | tail -1)" "gpu:lone"
# --device is unusable if there is no way to read the aliases out of the inventory.
check "-v lists the aliases the flag takes" \
  "$($T --machines -v | grep -c 'gpu:one')" "1"
check "with the bus id and what can reach it" \
  "$($T --machines -v | grep 'gpu:one' | grep -c '0000:07:00.0.*cuda')" "1"
check "and without -v it stays a machine list" \
  "$($T --machines | grep -c 'gpu:')" "0"
check "an unknown alias is refused" \
  "$($T --on rig --device gpu:nope --label dev 'echo no' 2>&1 >/dev/null | grep -c "no device called")" "1"
check "and says what the machine does have" \
  "$($T --on rig --device gpu:nope --label dev 'echo no' 2>&1 >/dev/null | grep -c 'gpu:one')" "1"
# Two cards of one model have one vendor:model between them, so the selector that keys on it
# names both. DRI_PRIME takes a PCI address instead, which is what tells them apart.
check "each of two identical cards gets its own address" \
  "$($T --on rig --device gpu:twin.03 --label dev 'printf %s "$DRI_PRIME"' 2>/dev/null | tail -1)" \
  "pci-0000_03_00_0"
check "and the other one gets the other" \
  "$($T --on rig --device gpu:twin.06 --label dev 'printf %s "$DRI_PRIME"' 2>/dev/null | tail -1)" \
  "pci-0000_06_00_0"
# The model selector is a layer above every ICD, so it reorders after DRI_PRIME has and wins.
# Where the model names two cards it picks whichever it likes, and setting the two together
# sent both halves of a pair to one card while each looked pinned. So: not for a twin.
check "the model selector stays out of the way of a twin" \
  "$($T --on rig --device gpu:twin.03 --label dev 'printf %s "${MESA_VK_DEVICE_SELECT:-unset}"' 2>/dev/null | tail -1)" \
  "unset"
# It is still needed where the model is unique, because DRI_PRIME is Mesa's and does nothing
# for the NVIDIA ICD.
check "and is used where the model names one card" \
  "$($T --on rig --device gpu:lone --label dev 'printf %s "$MESA_VK_DEVICE_SELECT"' 2>/dev/null | tail -1)" \
  "8086:b080"

# --on records the machine's name; DIBS_HOST names it by its ssh string and leaves that name
# empty, because a host string is not an inventory name. The alias was then looked up in the
# default machine's entry: refused when that machine had no such card, and worse when it did,
# since the address of a card in the wrong box resolves and pins nothing.
check "a device is read from the machine the call is going to" \
  "$(DIBS_HOST=other $T --device gpu:elsewhere --label dev 'printf %s "$DRI_PRIME"' 2>/dev/null | tail -1)" \
  "pci-0000_0a_00_0"
check "and a card the target does not have is refused" \
  "$(DIBS_HOST=other $T --device gpu:one --label dev 'echo ran' 2>&1 >/dev/null | grep -c 'no device called')" "1"
check "  naming the target rather than the default" \
  "$(DIBS_HOST=other $T --device gpu:one --label dev 'echo ran' 2>&1 >/dev/null | grep -c '^dibs: other has')" "1"
check "  and nothing ran on the wrong card" \
  "$(DIBS_HOST=other $T --device gpu:one --label dev 'echo ran' 2>/dev/null | grep -c ran)" "0"
# The unpinned-benchmark warning counted the default machine's cards too, so it stayed silent
# about a four-card machine whenever the default had one.
check "the unpinned warning counts the target's cards" \
  "$(DIBS_HOST=other $T --bench --label unp true 2>&1 >/dev/null | grep -c 'GPUs and this benchmark')" "0"
check "and still fires on a machine that has several" \
  "$(DIBS_HOST=rig $T --bench --label unp true 2>&1 >/dev/null | grep -c 'rig has 4 GPUs')" "1"

echo "one label, one series"
rm -f "$DIBS_SERIES"
# A label is the key a measurement's history is filed under, so two runs of it are meant to be
# two samples of one thing. Two cards make them two things, and nothing about the numbers says so.
# Two names for this machine, so a label can move between them without a second machine: what
# the check compares is the name the run was dispatched under, which is what a real move changes.
cat > "$DIBS_MACHINES" <<TOML
default = "alpha"

[machine.alpha]
ssh      = "dibs@alpha"
hostname = "$(hostname -s)"

[machine.beta]
ssh      = "dibs@beta"
hostname = "$(hostname -s)"
TOML
$T --on alpha --bench --label series-a 'echo one' >/dev/null 2>&1
check "the first benchmark under a label claims it" \
  "$(awk -F'\t' '$1=="series-a" {print "yes"}' "$DIBS_SERIES")" "yes"
check "running it again the same way is fine" \
  "$($T --on alpha --bench --label series-a 'echo two' >/dev/null 2>&1; echo $?)" "0"
check "a second machine under one label is refused" \
  "$($T --on beta --bench --label series-a 'echo no' 2>&1 >/dev/null | grep -c 'two histories')" "1"
check "and it names what the label was measured on before" \
  "$($T --on beta --bench --label series-a 'echo no' 2>&1 >/dev/null | grep -c 'before:  dibs@alpha')" "1"
# One machine reached under two names is one machine. Keying on the name it was called rather
# than on where it goes would record it twice and refuse a run that was never a move.
check "the same machine under another name is not a move" \
  "$(printf '#dibs-series 1\nsame\tdibs@alpha\tnone\tx\t1\n' > "$DIBS_SERIES"
     DIBS_HOST=dibs@alpha $T --bench --label same 'echo fine' >/dev/null 2>&1; echo $?)" "0"
$T --on alpha --bench --label series-a 'echo again' >/dev/null 2>&1
check "and refusing is an error, not a note" \
  "$($T --on beta --bench --label series-a 'echo no' >/dev/null 2>&1; echo $?)" "2"
# Suppressing the message and filing the new numbers beside the old ones would rebuild the
# mixed history the check exists to prevent, so a deliberate move starts the series over.
check "--new-series moves it rather than merging" \
  "$($T --on beta --bench --label series-a --new-series 'echo moved' >/dev/null 2>&1; echo $?)" "0"
check "so the old machine is now the odd one out" \
  "$($T --on alpha --bench --label series-a 'echo no' 2>&1 >/dev/null | grep -c 'two histories')" "1"
# A job that measured nothing must not claim the label: a first attempt that failed would
# otherwise pin every later run to wherever it happened to fail.
rm -f "$DIBS_SERIES"
$T --bench --label series-b 'exit 3' >/dev/null 2>&1
check "a benchmark that failed claims nothing" \
  "$(grep -c series-b "$DIBS_SERIES" 2>/dev/null || echo 0)" "0"
# Builds and tests do not care which card they did not use, and blocking one would make this
# an obstacle rather than a guard.
check "shared work is not checked at all" \
  "$($T --on beta --label series-a 'echo fine' >/dev/null 2>&1; echo $?)" "0"

echo "a transfer goes where it was told"
cat > "$DIBS_MACHINES" <<TOML
default = "wrongbox"

[machine.wrongbox]
ssh      = "dibs@wrongbox"
hostname = "wrongbox"

[machine.rightbox]
ssh      = "dibs@rightbox"
hostname = "rightbox"
TOML
# rsync reaches the machine through a second dibs, and that one parses its own arguments: it
# never saw --on, resolved the default, and the transfer went to the wrong machine silently
# and with exit 0. The resolved host rides in the environment, which the child inherits.
check "--sync carries --on to the transport it spawns" \
  "$(DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --on rightbox --sync ./x :~/y 2>&1 |
     grep -q 'dibs@rightbox' && echo reached || echo elsewhere)" "reached"
check "and does not fall back to the default machine" \
  "$(DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --on rightbox --sync ./x :~/y 2>&1 | grep -c 'wrongbox')" "0"
# Said before the bytes move. A transfer to the wrong machine succeeds, so the only moment it
# can be caught is before it happens.
check "and says where it is about to write" \
  "$(DIBS_LOCAL=0 DIBS_CONNECT_TIMEOUT=2 $T --on rightbox --sync ./x :~/y 2>&1 | grep -c 'syncing with dibs@rightbox')" "1"

echo "nothing left behind"
check "no holders" "$(holders)" "0"
check "no waiters" "$(waiters)" "0"
check "no cpu samples" "$(count_ cpu)" "0"
echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
