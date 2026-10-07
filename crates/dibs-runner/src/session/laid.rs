use crate::{
    clock::Deadline,
    job::{Environment, Output},
    platform::{Host, Platform as _},
    session::run::Venue,
    settings::{home, var},
    stop::Stage,
    tree::{BuildMark, Commands, Spot, Stepping, TreeConfig, Trees},
};
use dibs_format::{
    By, Exit, Mode, Pair, Pairs,
    wire::{Place, Record, Step, Stepped, Then, Tree},
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// What a job's tree came to before its command.
pub enum Laid {
    /// The command runs, in the tree when there is one, where a recipe step is a step.
    Run(Option<Spot>),
    /// Nothing more runs: the prepare was the job, failed, or waits for a git dependency.
    Done { status: i32, by: By },
}

/// A recipe step about to run: refused, or running with the machine's state when it is a
/// measurement.
pub enum Begins {
    Refused,
    Runs(Running),
}

/// A step's command running: the machine's state when it is a measurement, and a build's mark.
pub struct Running {
    pub state: Option<Pairs>,
    pub mark: Option<BuildMark>,
}

/// A job's tree: laid out before its command, and a recipe step begun and ended around it.
pub struct Layout<'a> {
    at: Venue<'a>,
    output: Output<'a>,
}

impl<'a> Layout<'a> {
    pub fn new(at: Venue<'a>, output: Output<'a>) -> Self {
        Layout { at, output }
    }

    /// The job's tree laid out, or found, and the command pointed at it.
    pub fn lay_out(&self, tree: &Tree, environment: &mut Environment, deadline: Deadline) -> Laid {
        let prepare = match &tree.place {
            Place::At(laid) => {
                environment.in_tree(PathBuf::from(&laid.worktree), Some(laid.target.clone()));
                return Laid::Run(Some(Spot {
                    worktree: PathBuf::from(&laid.worktree),
                    target: PathBuf::from(&laid.target),
                }));
            }
            Place::Prepare(prepare) => prepare,
        };
        let say = |text: &str| self.output.tell(self.at.sink, text);
        let running = |pid| self.at.stopper.state().stage = Stage::Preparing(pid);
        let unstopped = |act: &mut dyn FnMut()| {
            let _state = self.at.stopper.state();
            act();
        };
        let home = home();
        let cargo_home = var("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        let config = TreeConfig {
            scratch: &self.at.machine.scratch,
            home: &home,
            cargo_home: &cargo_home,
            clocks: self.at.settings.clocks,
            seed_wait: Duration::from_secs(self.at.settings.seed_wait),
            reflinks: self.at.settings.reflinks,
        };
        let commands = Commands::new(environment, &running, &unstopped, deadline);
        let prepared = Trees::new(config, commands, &say).prepare(prepare);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                say(&error.to_string());
                return Laid::Done {
                    status: error.exit().status(),
                    by: By::Dibs,
                };
            }
        };
        self.at
            .sink
            .record(Record::Prepared(Box::new(prepared.clone())));
        let missing = prepared.held();
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
                if self.at.call.mode() == Mode::Rsh {
                    self.at.sink.transferring();
                }
                Laid::Run(None)
            }
        }
    }

    /// What comes before a step's command: a measurement refused where another tree has built
    /// since, the machine's state, and a build's claim on its target.
    pub fn step_begins(&self, step: &Step, stepping: &Stepping) -> Begins {
        if step.check && stepping.refused() {
            self.at.sink.record(Record::Stepped(Stepped {
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
        Begins::Runs(Running {
            state,
            mark: step.record.as_ref().and_then(|_| stepping.building()),
        })
    }

    /// What comes after it: a build's lockfile recorded once it succeeded, the files it kept,
    /// and the pin check, which ends the step 3 when a pin did not take. Whether dibs gave the
    /// status.
    pub fn step_ends(
        &self,
        step: &Step,
        stepping: &Stepping,
        status: &mut i32,
        running: Running,
    ) -> bool {
        if let Some(mark) = running.mark {
            mark.ended(*status);
        }
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
        self.at.sink.record(Record::Stepped(Stepped {
            refused: false,
            state: running.state,
            artifacts,
        }));
        unpinned
    }
}
