use crate::worktree;
use std::path::Path;

/// What `@<ref>` names, before anything is looked up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Side {
    Local,
    /// Fetched on the machine by this name.
    Ref(String),
    /// The tip of a range: resolved here, so the merge base and the arm come from one history.
    Pinned(String),
    /// Where the second left the first.
    Base(String, String),
}

impl Side {
    pub(crate) fn name(&self) -> String {
        match self {
            Side::Local => "local".into(),
            Side::Ref(r) | Side::Pinned(r) => r.clone(),
            Side::Base(..) => "base".into(),
        }
    }

    /// Sent from here whatever the remote holds: the tree as it stands, or the base it is measured
    /// against.
    pub(crate) fn sent(&self) -> bool {
        match self {
            Side::Local => true,
            Side::Base(_, tip) => tip == "local",
            Side::Ref(_) | Side::Pinned(_) => false,
        }
    }
}

/// One tree, `A..B`, or `a,b,c`.
pub(crate) fn sides(reference: Option<&str>) -> Result<Vec<Side>, String> {
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
pub(crate) struct Arm {
    pub(crate) name: String,
    /// What the machine fetches, or None for a tree sent from here.
    pub(crate) fetch: Option<String>,
    /// How its commit was found, for a person to check.
    pub(crate) note: Option<String>,
    /// A commit sent from a checkout of its own rather than fetched.
    pub(crate) checkout: Option<worktree::Checkout>,
}

impl Arm {
    /// Where a sent arm is sent from.
    pub(crate) fn dir<'a>(&'a self, checkout: &'a Path) -> &'a Path {
        self.checkout.as_ref().map_or(checkout, |c| c.dir.as_path())
    }

    pub(crate) fn local(&self, checkout: &Path) -> Result<worktree::Local, String> {
        match &self.checkout {
            Some(c) => c.local(),
            None => worktree::local(checkout),
        }
    }

    pub(crate) fn commit(&self) -> Option<&str> {
        self.checkout
            .as_ref()
            .map(|c| c.sha.as_str())
            .or(self.fetch.as_deref())
    }
}

/// `, <note>, sent from <from> since <why>`, as much of it as there is, to follow the commit.
pub(crate) fn sent_from(c: &worktree::Checkout, note: Option<&str>, from: &str) -> String {
    format!(
        "{}, sent from {from}{}",
        note.map(|n| format!(", {n}")).unwrap_or_default(),
        c.why.map(|w| format!(" since {w}")).unwrap_or_default()
    )
}

pub(crate) fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

/// Each side looked up here. A commit the machine cannot fetch is checked out and sent instead.
pub(crate) fn arms(sides: &[Side], dir: &Path, repo: &str) -> Result<Vec<Arm>, String> {
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
            Side::Ref(r) => match worktree::as_fetched(dir, r) {
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
                let sha = worktree::commit(dir, r)?;
                (sha.clone(), sha, None, None)
            }
            Side::Base(a, b) => {
                let (sha, upstream) = worktree::merge_base(dir, &here(a), &here(b))?;
                let note = match upstream {
                    Some(u) => format!("where {b} left {u}, since {a} is behind it"),
                    None => format!("where {b} left {a}"),
                };
                (sha.clone(), sha, Some(note), None)
            }
        };
        let why = match s.sent() {
            true => None,
            false => ahead.or_else(|| worktree::unfetchable(dir, &sha)),
        };
        arms.push(match s.sent() || why.is_some() {
            true => Arm {
                name,
                fetch: None,
                note,
                checkout: Some(worktree::checkout(dir, repo, &sha, why)?),
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
