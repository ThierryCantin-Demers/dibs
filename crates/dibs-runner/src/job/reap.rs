use crate::platform::{Host, Platform as _, Process};
use std::{collections::BTreeSet, thread, time::Duration};

const ROUND: Duration = Duration::from_millis(250);
/// Rounds of TERM to what is below, then of KILL, before the named processes themselves.
const BELOW_ROUNDS: usize = 50;
const BELOW_TERM_ROUNDS: usize = 40;
/// Rounds the named processes are given to end after TERM before KILL.
const NAMED_ROUNDS: usize = 41;

/// The processes under a pid, parents before children, the pid itself left out.
pub fn tree_below(root: u32, processes: &[Process]) -> Vec<u32> {
    let mut wanted = BTreeSet::from([root]);
    let mut below = Vec::new();
    let mut level = vec![root];
    while !level.is_empty() {
        let next: Vec<u32> = processes
            .iter()
            .filter(|p| level.contains(&p.parent) && !wanted.contains(&p.pid))
            .map(|p| p.pid)
            .collect();
        wanted.extend(next.iter().copied());
        below.extend(next.iter().copied());
        level = next;
    }
    below
}

fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: kill only sends a signal.
    unsafe { libc::kill(pid as libc::pid_t, signal) };
}

/// Stops a job's whole tree. The lock goes the moment the job exits, and a grandchild still running
/// then would run unlocked beside the next measurement, so everything below goes first, deepest
/// first, then what was named: TERM, and KILL for whatever outlives it.
pub fn reap(pids: &[u32]) {
    for round in 1..=BELOW_ROUNDS {
        let processes = Host::processes();
        let below: Vec<u32> = pids
            .iter()
            .flat_map(|p| tree_below(*p, &processes))
            .collect();
        if below.is_empty() {
            break;
        }
        let sig = match round > BELOW_TERM_ROUNDS {
            true => libc::SIGKILL,
            false => libc::SIGTERM,
        };
        for pid in below.iter().rev() {
            signal(*pid, sig);
        }
        thread::sleep(ROUND);
    }
    for pid in pids {
        signal(*pid, libc::SIGTERM);
    }
    for round in 1..=NAMED_ROUNDS {
        let alive: Vec<u32> = pids.iter().copied().filter(|p| Host::running(*p)).collect();
        if alive.is_empty() {
            return;
        }
        if round == NAMED_ROUNDS {
            for pid in alive {
                signal(pid, libc::SIGKILL);
            }
        }
        thread::sleep(ROUND);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_below_lists_parents_before_children() {
        let p = |pid, parent| Process { pid, parent };
        let table = [p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(13, 10), p(20, 1)];
        assert_eq!(tree_below(10, &table), vec![11, 13, 12]);
        assert!(tree_below(12, &table).is_empty());
    }
}
