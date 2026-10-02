use crate::{
    call::{
        base::{CallError, LockedCall},
        kill::Kill,
        machine::{Asked, MachineCall},
        sync::{Rsh, Sync},
    },
    caller::Caller,
    cli::{Call, Mode as Words},
    placement::Placement,
    update::Update,
};
use dibs_format::{Label, Mode};
use std::fmt;

const GC_LABEL: &str = "dibs-gc";

/// What `--gc` asks the machine to sweep, in the command's place: its clocks, and whether only
/// to list.
struct GcSweep {
    /// None leaves the machine's own clocks.
    days: Option<u32>,
    dry_run: bool,
}

impl fmt::Display for GcSweep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.days {
            Some(days) => write!(f, "{days}")?,
            None => write!(f, "default")?,
        }
        write!(f, " {}", u8::from(self.dry_run))
    }
}

/// A parsed call, answered by the mode it names.
pub struct Dispatch<'a> {
    pub call: &'a Call,
    pub caller: &'a Caller,
}

impl Dispatch<'_> {
    /// The call's exit, with what refused it said on stderr.
    pub fn exit(&self) -> i32 {
        self.answer().unwrap_or_else(|e| {
            eprint!("{e}");
            e.exit()
        })
    }

    fn answer(&self) -> Result<i32, CallError> {
        let machine = MachineCall::new(self.call, self.caller);
        match &self.call.mode {
            Words::Run(run) => LockedCall::run_of(run).run(self.call, self.caller),
            Words::Peek(command) => LockedCall::peek(command).run(self.call, self.caller),
            Words::Status => machine.status(),
            Words::Watch { every } => {
                machine.answer(Asked::plain(Mode::Watch, Label::new(every.to_string())))
            }
            Words::Log { lines } => {
                machine.answer(Asked::plain(Mode::Log, Label::new(lines.to_string())))
            }
            Words::Release => machine.answer(Asked::plain(Mode::Release, machine.label())),
            Words::Gc { days, dry_run } => machine.answer(Asked {
                mode: Mode::Gc,
                label: self
                    .call
                    .label
                    .clone()
                    .unwrap_or_else(|| Label::new(GC_LABEL)),
                command: GcSweep {
                    days: *days,
                    dry_run: *dry_run,
                }
                .to_string(),
                streamed: true,
            }),
            Words::Check { host } => machine.check(host.as_deref()),
            Words::Out(target) => machine.out(target.as_ref()),
            Words::Fetch { job, into } => machine.fetch(job, into.as_deref()),
            Words::Kill {
                target,
                force,
                anyone,
            } => Kill {
                machine: &machine,
                force: *force,
                anyone: *anyone,
            }
            .answer(target),
            Words::Sync(args) => Sync {
                machine: &machine,
                args,
            }
            .answer(),
            Words::Rsh { command, .. } => Rsh {
                machine: &machine,
                command,
            }
            .answer(),
            Words::Machines => machine.machines(),
            Words::Which => machine.which(),
            Words::Forget(name) => machine.forget(name),
            Words::Pick => match (Placement { machine: &machine }).pick() {
                Ok(placed) => {
                    println!("{placed}");
                    Ok(0)
                }
                Err(unplaced) => Err(unplaced.into()),
            },
            Words::Update => Ok(Update::of_this_build().run()),
        }
    }
}
