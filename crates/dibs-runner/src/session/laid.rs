use crate::{
    job::{Environment, Output},
    platform::{Host, Platform as _},
    session::{base::Session, run::Place},
    settings::{home, var},
    stop::Stage,
    tree::{Commands, Copier, Stepping, Trees},
};
use dibs_format::{
    By, Exit, Mode, Pair, Pairs,
    wire::{Place as Where, Record, Step, Stepped, Then, Tree},
};
use std::{
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

/// What a job's tree came to before its command.
pub(super) enum Laid {
    /// The command runs, in the tree when there is one, where a recipe step is a step.
    Run(Option<Spot>),
    /// Nothing more runs: the prepare was the job, failed, or waits for a git dependency.
    Done { status: i32, by: By },
}

/// A recipe step about to run: refused, or running with the machine's state when it is a
/// measurement.
pub(super) enum Begins {
    Refused,
    Runs(Option<Pairs>),
}

/// Where a recipe step runs: its tree, and the target it builds into.
pub(super) struct Spot {
    pub(super) worktree: PathBuf,
    pub(super) target: PathBuf,
}

impl Session {
    /// The job's tree laid out, or found, and the command pointed at it.
    pub(super) fn lay_out(
        &self,
        at: &Place,
        tree: &Tree,
        environment: &mut Environment,
        output: Output,
    ) -> Laid {
        let prepare = match &tree.place {
            Where::At(laid) => {
                environment.in_tree(PathBuf::from(&laid.worktree), Some(laid.target.clone()));
                return Laid::Run(Some(Spot {
                    worktree: PathBuf::from(&laid.worktree),
                    target: PathBuf::from(&laid.target),
                }));
            }
            Where::Prepare(prepare) => prepare,
        };
        let say = |text: &str| self.told(output, text);
        let running = |pid| at.stopper.state().stage = Stage::Preparing(pid);
        let home = home();
        let cargo_home = var("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        let prepared = Trees {
            scratch: &at.machine.scratch,
            home: &home,
            cargo_home: &cargo_home,
            keep_days: self.settings.keep_days,
            target_keep_days: self.settings.target_keep_days,
            seed_wait: Duration::from_secs(self.settings.seed_wait),
            copier: Copier {
                reflinks: self.settings.reflinks,
            },
            commands: Commands {
                environment,
                running: &running,
            },
            say: &say,
        }
        .prepare(prepare);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(exit) => {
                return Laid::Done {
                    status: exit.status(),
                    by: By::Dibs,
                };
            }
        };
        self.sink
            .record(Record::Prepared(Box::new(prepared.clone())));
        let missing = prepared
            .gitdbs
            .as_ref()
            .is_some_and(|g| !g.missing.is_empty());
        let worktree = PathBuf::from(&prepared.worktree);
        match tree.then {
            Then::Nothing => Laid::Done {
                status: 0,
                by: By::Command,
            },
            Then::Step if missing => {
                say("dibs: a git dependency has to be sent first; the step runs once it is\n");
                Laid::Done {
                    status: Exit::Setup.status(),
                    by: By::Dibs,
                }
            }
            Then::Step => {
                environment.in_tree(worktree.clone(), Some(prepared.target.clone()));
                Laid::Run(Some(Spot {
                    worktree,
                    target: PathBuf::from(prepared.target),
                }))
            }
            // The transfer names the tree's directory, so a tree never prepared cannot turn
            // `--delete` on wherever the job happened to start.
            Then::Transfer => {
                environment.in_tree(
                    worktree.parent().unwrap_or(Path::new("/")).to_path_buf(),
                    None,
                );
                if self.call.mode() == Mode::Rsh {
                    self.sink.transferring();
                }
                Laid::Run(None)
            }
        }
    }

    /// What comes before a step's command: a measurement refused where another tree has built
    /// since, the machine's state, and a build's claim on its target.
    pub(super) fn step_begins(&self, step: &Step, stepping: &Stepping) -> Begins {
        if step.check && stepping.refused() {
            self.sink.record(Record::Stepped(Stepped {
                refused: true,
                ..Stepped::default()
            }));
            return Begins::Refused;
        }
        let state = step.state.then(|| {
            Pairs(
                Host::machine_state()
                    .split_whitespace()
                    .filter_map(|kv| kv.split_once('='))
                    .filter(|(_, value)| !value.is_empty())
                    .map(|(name, value)| Pair {
                        name: name.to_string(),
                        value: value.to_string(),
                    })
                    .collect(),
            )
        });
        if step.claim {
            stepping.claim();
        }
        Begins::Runs(state)
    }

    /// What comes after it: a build's lockfile recorded once it succeeded, the files it kept,
    /// and the pin check, which ends the step 3 when a pin did not take. Whether dibs gave the
    /// status.
    pub(super) fn step_ends(
        &self,
        step: &Step,
        stepping: &Stepping,
        status: &mut i32,
        state: Option<Pairs>,
    ) -> bool {
        if let Some(token) = &step.record
            && *status == 0
        {
            stepping.record(token);
        }
        let artifacts = stepping.keep(&step.artifacts);
        let unpinned = !step.pinned.is_empty() && stepping.unpinned(&step.pinned);
        if unpinned {
            *status = Exit::Setup.status();
        }
        self.sink.record(Record::Stepped(Stepped {
            refused: false,
            state,
            artifacts,
        }));
        unpinned
    }

    /// What a prepare says, where the job's own output goes.
    pub(super) fn told(&self, output: Output, text: &str) {
        if text.is_empty() {
            return;
        }
        let logged = |log: &Path| {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
                let _ = file.write_all(text.as_bytes());
            }
        };
        match output {
            Output::Log(log) => logged(log),
            Output::Stream(log) => {
                logged(log);
                self.sink.out(text.as_bytes());
            }
            Output::Caller | Output::Through => self.sink.say(text),
        }
    }
}
