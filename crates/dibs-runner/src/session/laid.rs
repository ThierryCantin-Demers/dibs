use crate::{
    job::{Environment, Output},
    session::{base::Session, run::Place},
    settings::{home, var},
    stop::Stage,
    tree::{Commands, Copier, Trees},
};
use dibs_format::{
    By, Exit, Mode,
    wire::{Place as Where, Record, Then, Tree},
};
use std::{
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

/// What a job's tree came to before its command.
pub(super) enum Laid {
    /// The command runs, in the tree when there is one.
    Run,
    /// Nothing more runs: the prepare was the job, failed, or waits for a git dependency.
    Done { status: i32, by: By },
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
                return Laid::Run;
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
                environment.in_tree(worktree, Some(prepared.target));
                Laid::Run
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
                Laid::Run
            }
        }
    }

    /// What a prepare says, where the job's own output goes.
    fn told(&self, output: Output, text: &str) {
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
