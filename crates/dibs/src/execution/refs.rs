use crate::git::Git;
use std::path::Path;

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

/// One tree, `A..B`, or `a,b,c`.
pub fn sides(reference: Option<&str>) -> Result<Vec<Side>, String> {
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
        return Err(format!(
            "{r}: A...B is not a comparison dibs makes. A..B measures B against where it left A"
        ));
    }
    if let Some((a, b)) = r.split_once("..") {
        if a.is_empty() || b.is_empty() || b.contains("..") || r.contains(',') {
            return Err(format!(
                "{r}: a range names both ends, as main..local. Several arms in turn are a,b,c"
            ));
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
        return Err(format!("{r}: an empty arm"));
    }
    if let Some(twice) = list
        .iter()
        .enumerate()
        .find(|(i, s)| list[..*i].contains(s))
    {
        return Err(format!("{r}: {} is named twice", twice.1.name()));
    }
    Ok(list)
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

    pub fn local(&self, checkout: &Path) -> Result<super::Local, String> {
        match &self.checkout {
            Some(c) => c.local(),
            None => super::local(checkout),
        }
    }

    pub fn commit(&self) -> Option<&str> {
        self.checkout
            .as_ref()
            .map(|c| c.sha.as_str())
            .or(self.fetch.as_deref())
    }
}

/// `, <note>, sent from <from> since <why>`, as much of it as there is, to follow the commit.
pub fn sent_from(c: &super::Checkout, note: Option<&str>, from: &str) -> String {
    format!(
        "{}, sent from {from}{}",
        note.map(|n| format!(", {n}")).unwrap_or_default(),
        c.why.map(|w| format!(" since {w}")).unwrap_or_default()
    )
}

pub fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

/// Each side looked up here. A commit the machine cannot fetch is checked out and sent instead.
pub fn arms(sides: &[Side], dir: &Path, repo: &str) -> Result<Vec<Arm>, String> {
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
            Side::Ref(r) => match super::as_fetched(dir, r) {
                Some((sha, seen, ahead)) => (
                    sha,
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
                let sha = super::commit(dir, r)?;
                (sha.clone(), sha, None, None)
            }
            Side::Base(a, b) => {
                let (sha, upstream) = super::merge_base(dir, &here(a), &here(b))?;
                let note = match upstream {
                    Some(u) => format!("where {b} left {u}, since {a} is behind it"),
                    None => format!("where {b} left {a}"),
                };
                (sha.clone(), sha, Some(note), None)
            }
        };
        let why = match s.sent() {
            true => None,
            false => ahead.or_else(|| super::unfetchable(dir, &sha)),
        };
        arms.push(match s.sent() || why.is_some() {
            true => Arm {
                name,
                fetch: None,
                note,
                checkout: Some(super::checkout(dir, repo, &sha, why)?),
            },
            // A ref is fetched by name, so how it stands here says nothing of what the machine takes.
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
        return Err(format!(
            "{} has nothing its base does not, so there is nothing to compare",
            tip.name
        ));
    }
    Ok(arms)
}

/// The commit `name` is here, in full.
pub fn commit(dir: &std::path::Path, name: &str) -> Result<String, String> {
    Git(dir)
        .run(&["rev-parse", "--verify", "-q", &format!("{name}^{{commit}}")])
        .map(|s| s.trim().to_string())
        .map_err(|_| format!("no {name} in {}", dir.display()))
}

/// Where `tip` left `from`: the commit an A/B of `from..tip` measures `tip` against. A local branch
/// that is behind its upstream would put that point too early and credit `tip` with commits it
/// merely did not have, so the upstream is asked too and the later of the two answers wins.
/// Returns the commit and, when the upstream decided it, the upstream's name.
pub fn merge_base(
    dir: &std::path::Path,
    from: &str,
    tip: &str,
) -> Result<(String, Option<String>), String> {
    let tip = commit(dir, tip)?;
    let own = commit(dir, from)?;
    let base = |c: &str| {
        Git(dir)
            .run(&["merge-base", c, &tip])
            .map(|s| s.trim().to_string())
    };
    let mine = base(&own)
        .map_err(|_| format!("{from} and {tip:.8} share no history in {}", dir.display()))?;
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
        .and_then(|u| Some((u.to_string(), base(&commit(dir, u).ok()?).ok()?)));
    match theirs {
        Some((u, b))
            if b != mine
                && Git(dir)
                    .run(&["merge-base", "--is-ancestor", &mine, &b])
                    .is_ok() =>
        {
            Ok((b, Some(u)))
        }
        _ => Ok((mine, None)),
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
            merge_base(&r, "main", "feat").unwrap(),
            (c2.clone(), Some("origin/main".to_string()))
        );
        assert_eq!(
            merge_base(&r, "origin/main", "HEAD").unwrap(),
            (c2.clone(), None)
        );
        assert_eq!(
            merge_base(&r, &c1, "feat").unwrap(),
            (c1, None),
            "a commit has no upstream to ask"
        );
        assert!(merge_base(&r, "no-such", "feat").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
