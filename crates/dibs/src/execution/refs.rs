use super::{
    error::ArmError,
    local::{Fetched, Repo},
};
use crate::{
    git::{Git, GitError},
    recipe::Lock,
};
use dibs_format::{JobId, StepRecord, wire::Revision};
use std::{collections::BTreeMap, path::Path};

/// What `@<ref>` names, before anything is looked up.
#[derive(Debug, Clone, PartialEq)]
pub enum Side {
    Local,
    /// Fetched on the machine by this name.
    Ref(String),
    /// The tip of a range: resolved here, so the merge base and the arm come from one history.
    Pinned(String),
    /// Where the second left the first.
    Base(String, String),
}

impl Side {
    /// One tree, `A..B`, or `a,b,c`.
    pub fn list(reference: Option<&str>) -> Result<Vec<Side>, ArmError> {
        let one = |r: &str| {
            if r == "local" {
                Side::Local
            } else {
                Side::Ref(r.to_string())
            }
        };
        let Some(r) = reference else {
            return Ok(vec![Side::Ref("HEAD".into())]);
        };
        if r.contains("...") {
            return Err(ArmError::Symmetric(r.to_string()));
        }
        if let Some((a, b)) = r.split_once("..") {
            if a.is_empty() || b.is_empty() || b.contains("..") || r.contains(',') {
                return Err(ArmError::HalfRange(r.to_string()));
            }
            let tip = if b == "local" {
                Side::Local
            } else {
                Side::Pinned(b.to_string())
            };
            return Ok(vec![Side::Base(a.into(), b.into()), tip]);
        }
        let list: Vec<Side> = r.split(',').map(one).collect();
        if list.iter().any(|s| s.name().is_empty()) {
            return Err(ArmError::EmptyArm(r.to_string()));
        }
        if let Some(twice) = list
            .iter()
            .enumerate()
            .find(|(i, s)| list[..*i].contains(s))
        {
            return Err(ArmError::Twice {
                reference: r.to_string(),
                arm: twice.1.name(),
            });
        }
        Ok(list)
    }

    pub fn name(&self) -> String {
        match self {
            Side::Local => "local".into(),
            Side::Ref(r) | Side::Pinned(r) => r.clone(),
            Side::Base(..) => "base".into(),
        }
    }

    /// Sent from here whatever the remote holds: the tree as it stands, or the base it is measured
    /// against.
    pub fn sent(&self) -> bool {
        match self {
            Side::Local => true,
            Side::Base(_, tip) => tip == "local",
            Side::Ref(_) | Side::Pinned(_) => false,
        }
    }
}

/// One side of a comparison, looked up.
pub struct Arm {
    pub name: String,
    /// What the machine fetches, or None for a tree sent from here.
    pub fetch: Option<String>,
    /// How its commit was found, for a person to check.
    pub note: Option<String>,
    /// A commit sent from a checkout of its own rather than fetched.
    pub checkout: Option<super::Checkout>,
}

impl Arm {
    /// Where a sent arm is sent from.
    pub fn dir<'a>(&'a self, checkout: &'a Path) -> &'a Path {
        self.checkout.as_ref().map_or(checkout, |c| c.dir.as_path())
    }

    pub fn local(&self, checkout: &Path) -> Result<super::Local, GitError> {
        match &self.checkout {
            Some(c) => c.local(),
            None => super::Local::of(checkout),
        }
    }

    pub fn commit(&self) -> Option<&str> {
        self.checkout
            .as_ref()
            .map(|c| c.sha.as_str())
            .or(self.fetch.as_deref())
    }

    /// Each side looked up here. A commit the machine cannot fetch is checked out and sent instead.
    pub fn look_up(sides: &[Side], dir: &Path, repo: &str) -> Result<Vec<Arm>, ArmError> {
        let here = |r: &str| {
            if r == "local" {
                "HEAD".to_string()
            } else {
                r.to_string()
            }
        };
        let mut arms = Vec::with_capacity(sides.len());
        for s in sides {
            let name = s.name();
            let (sha, fetch, note, ahead) = match s {
                Side::Local => {
                    arms.push(Arm {
                        name,
                        fetch: None,
                        note: None,
                        checkout: None,
                    });
                    continue;
                }
                Side::Ref(r) => match Fetched::of(dir, r) {
                    Some(Fetched {
                        commit,
                        seen,
                        ahead,
                    }) => (
                        commit,
                        r.clone(),
                        Some(format!("as {seen} stands here")),
                        ahead,
                    ),
                    None => {
                        arms.push(Arm {
                            name,
                            fetch: Some(r.clone()),
                            note: None,
                            checkout: None,
                        });
                        continue;
                    }
                },
                Side::Pinned(r) => {
                    let sha = Repo(dir).commit(r)?;
                    (sha.clone(), sha, None, None)
                }
                Side::Base(a, b) => {
                    let Base { commit, upstream } = Base::of(dir, &here(a), &here(b))?;
                    let note = match upstream {
                        Some(u) => format!("where {b} left {u}, since {a} is behind it"),
                        None => format!("where {b} left {a}"),
                    };
                    (commit.clone(), commit, Some(note), None)
                }
            };
            let why = match s.sent() {
                true => None,
                false => ahead.or_else(|| Repo(dir).unfetchable(&sha)),
            };
            arms.push(match s.sent() || why.is_some() {
                true => Arm {
                    name,
                    fetch: None,
                    note,
                    checkout: Some(super::Checkout::of(dir, repo, &sha, why)?),
                },
                // A ref is fetched by name, so how it stands here says nothing of what the machine
                // takes.
                false => Arm {
                    name,
                    fetch: Some(fetch),
                    note: note.filter(|_| !matches!(s, Side::Ref(_))),
                    checkout: None,
                },
            });
        }
        if let ([Side::Base(..), _], [base, tip]) = (sides, &arms[..])
            && base.commit().is_some()
            && base.commit() == tip.commit()
        {
            return Err(ArmError::NothingToCompare {
                tip: tip.name.clone(),
            });
        }
        Ok(arms)
    }

    /// Each arm's measured seconds per rep, and the jobs whose logs hold its numbers. The seconds
    /// are how long the steps held the lock, which is only a first look: the recipe's own output
    /// is the result.
    pub fn measured_summary(
        arms: &[Arm],
        steps: &[StepRecord],
        revisions: &dyn Fn(usize) -> Vec<Revision>,
    ) -> String {
        let width = arms.iter().map(|a| a.name.len()).max().unwrap_or(0);
        let mut out = String::from(
            "dibs: measured, each rep's exclusive seconds and the jobs with its output:\n",
        );
        for (a, arm) in arms.iter().enumerate() {
            let mine: Vec<&StepRecord> = steps
                .iter()
                .filter(|s| {
                    s.lock == Lock::Exclusive
                        && (arms.len() == 1 || s.arm.as_deref() == Some(arm.name.as_str()))
                })
                .collect();
            let mut per_rep: BTreeMap<u32, u64> = BTreeMap::new();
            for s in &mine {
                *per_rep.entry(s.rep.unwrap_or(1)).or_default() += s.seconds;
            }
            let secs: Vec<String> = per_rep.values().map(|s| format!("{s}s")).collect();
            let jobs: Vec<&str> = mine
                .iter()
                .filter_map(|s| s.job.as_ref().map(JobId::as_str))
                .collect();
            let revs: Vec<String> = revisions(a)
                .iter()
                .map(|r| format!("{}@{}", r.repo, r.sha))
                .collect();
            out += &format!(
                "  {:<width$}  {}  {}  jobs {}\n",
                arm.name,
                revs.join(" "),
                if secs.is_empty() {
                    "nothing measured".to_string()
                } else {
                    secs.join(" ")
                },
                if jobs.is_empty() {
                    "-".to_string()
                } else {
                    jobs.join(" ")
                }
            );
        }
        out
    }

    /// What is about to be prepared, for the person reading along.
    pub fn preparing(&self, repo: &str, local: Option<&super::Local>, dir: &Path) -> String {
        match (&self.checkout, local) {
            (Some(c), _) => format!(
                "{repo} at {}{}",
                c.short_sha(),
                c.sent_from(self.note.as_deref(), "this computer")
            ),
            (None, Some(l)) => format!(
                "{repo} from {} ({})",
                dir.display(),
                if l.dirty {
                    "uncommitted changes included"
                } else {
                    "clean"
                }
            ),
            (None, None) => format!(
                "{repo}@{}{}",
                self.fetch.as_deref().unwrap_or_default(),
                self.note
                    .as_ref()
                    .map(|n| format!(", {n}"))
                    .unwrap_or_default()
            ),
        }
    }
}

/// Where a range's tip left its base.
#[derive(Debug, PartialEq)]
pub struct Base {
    pub commit: String,
    /// The upstream that decided it, when the local branch is behind.
    pub upstream: Option<String>,
}

impl Base {
    /// Where `tip` left `from`: the commit an A/B of `from..tip` measures `tip` against. A local
    /// branch that is behind its upstream would put that point too early and credit `tip` with
    /// commits it merely did not have, so the upstream is asked too and the later of the two
    /// answers wins.
    pub fn of(dir: &Path, from: &str, tip: &str) -> Result<Base, ArmError> {
        let tip = Repo(dir).commit(tip)?;
        let own = Repo(dir).commit(from)?;
        let base = |c: &str| {
            Git(dir)
                .run(&["merge-base", c, &tip])
                .map(|s| s.trim().to_string())
        };
        let mine = base(&own).map_err(|_| ArmError::NoHistory {
            from: from.to_string(),
            tip: tip.clone(),
            dir: dir.to_path_buf(),
        })?;
        let upstream = Git(dir)
            .run(&[
                "rev-parse",
                "--abbrev-ref",
                "-q",
                &format!("{from}@{{upstream}}"),
            ])
            .ok()
            .map(|s| s.trim().to_string());
        let theirs = upstream
            .as_deref()
            .and_then(|u| Some((u.to_string(), base(&Repo(dir).commit(u).ok()?).ok()?)));
        match theirs {
            Some((u, b))
                if b != mine
                    && Git(dir)
                        .run(&["merge-base", "--is-ancestor", &mine, &b])
                        .is_ok() =>
            {
                Ok(Base {
                    commit: b,
                    upstream: Some(u),
                })
            }
            _ => Ok(Base {
                commit: mine,
                upstream: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A local main behind origin/main puts the base before the branch's real fork point, and the
    // branch is then credited with everything main gained in between.
    #[test]
    fn a_merge_base_is_taken_from_the_upstream_when_the_branch_is_behind_it() {
        let home = std::env::temp_dir().join(format!("dibs-mb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let sh = |cmd: &str| -> String {
            let o = std::process::Command::new("bash")
                .arg("-c")
                .arg(cmd)
                .current_dir(&home)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{cmd}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        sh("git init -q --bare origin.git && git clone -q origin.git r 2>/dev/null");
        let r = home.join("r");
        let git = |cmd: &str| {
            sh(&format!(
                "cd r && git -c user.email=a@b -c user.name=t {cmd}"
            ))
        };
        git("checkout -q -b main");
        git("commit -q --allow-empty -m c1");
        git("push -q -u origin main");
        let c1 = git("rev-parse HEAD");
        git("commit -q --allow-empty -m c2");
        git("push -q origin main");
        let c2 = git("rev-parse HEAD");
        git("reset -q --hard HEAD~1");
        git("checkout -q -b feat origin/main");
        git("commit -q --allow-empty -m f1");
        assert_eq!(
            Base::of(&r, "main", "feat").unwrap(),
            Base {
                commit: c2.clone(),
                upstream: Some("origin/main".to_string())
            }
        );
        assert_eq!(
            Base::of(&r, "origin/main", "HEAD").unwrap(),
            Base {
                commit: c2.clone(),
                upstream: None
            }
        );
        assert_eq!(
            Base::of(&r, &c1, "feat").unwrap(),
            Base {
                commit: c1,
                upstream: None
            },
            "a commit has no upstream to ask"
        );
        assert!(Base::of(&r, "no-such", "feat").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
