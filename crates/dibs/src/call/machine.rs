use crate::{
    call::base::{CallError, Request},
    caller::{Caller, short_hostname},
    cli::Call,
    machine::{
        Answer, CallValues, Card, Fleet, Here, Kept, Liveness, MachineHalf, Session, Target,
        TargetEnv, exit_code,
    },
    paths::Paths,
};
use dibs_format::{Label, MachineName, Mode};
use std::time::Duration;

/// A call in a mode the machine half answers itself, rather than by running a command there.
pub struct MachineCall<'a> {
    pub call: &'a Call,
    pub caller: &'a Caller,
    pub paths: Paths,
    pub fleet: Fleet,
    pub here: Here,
}

/// What one mode puts in the values the machine half reads.
pub struct Asked {
    pub mode: Mode,
    pub label: Label,
    pub command: String,
    /// The whole output rather than its digest.
    pub streamed: bool,
}

impl Asked {
    pub fn plain(mode: Mode, label: Label) -> Asked {
        Asked {
            mode,
            label,
            command: String::new(),
            streamed: false,
        }
    }
}

impl<'a> MachineCall<'a> {
    pub fn new(call: &'a Call, caller: &'a Caller) -> MachineCall<'a> {
        let paths = Paths::from_env();
        let fleet = Fleet::load(paths.inventory());
        MachineCall {
            call,
            caller,
            paths,
            fleet,
            here: Here {
                name: short_hostname(None),
                local: TargetEnv::from_env().local,
            },
        }
    }

    /// The machine the call names, or the one a setup with a single machine has.
    pub fn target(&self) -> Result<Target, CallError> {
        Ok(Target::resolve(
            self.call.on.as_ref(),
            &TargetEnv::from_env(),
            &self.fleet,
        )?)
    }

    /// Refused when the call names no machine and does not run on this computer.
    pub fn somewhere(&self, target: &Target) -> Result<(), CallError> {
        match target.host.is_empty() && !self.here.local {
            true => Err(target.no_machine(&self.fleet, false).into()),
            false => Ok(()),
        }
    }

    pub fn label(&self) -> Label {
        Request::label(self.call)
    }

    pub fn values(&self, asked: Asked, target: &Target) -> Result<CallValues, CallError> {
        self.values_for(self.call, asked, target)
    }

    fn values_for(
        &self,
        call: &Call,
        asked: Asked,
        target: &Target,
    ) -> Result<CallValues, CallError> {
        let card = match &call.device {
            Some(alias) => Card::resolve(alias, target, &self.fleet)?,
            None => Card::none(),
        };
        let values = Request {
            mode: asked.mode,
            call,
            caller: self.caller,
        }
        .values(asked.label.filed(), card, asked.command);
        Ok(match asked.streamed {
            true => CallValues {
                stream: "1".into(),
                ..values
            },
            false => values,
        })
    }

    /// The machine the call names, sent the mode's values.
    pub fn answer(&self, asked: Asked) -> Result<i32, CallError> {
        let target = self.target()?;
        self.somewhere(&target)?;
        self.send(asked, &target)
    }

    /// Sends the mode's values, and gives the call's exit.
    pub fn send(&self, asked: Asked, target: &Target) -> Result<i32, CallError> {
        let values = self.values(asked, target)?;
        if self.call.preflight {
            return Ok(0);
        }
        let session = Session::new(target, &self.here);
        let half = MachineHalf::load()?;
        let status = exit_code(session.run(&values, &half, Liveness::from_env())?);
        Ok(session.exit(status, target))
    }

    /// Sends the mode's values and keeps what the machine prints, saying why on stderr when it
    /// could not be reached.
    pub fn capture(&self, asked: Asked, target: &Target) -> Result<Answer, CallError> {
        let values = CallValues {
            tty: false,
            ..self.values(asked, target)?
        };
        let session = Session::new(target, &self.here);
        let half = MachineHalf::load()?;
        let answer = session.ask(&values, &half, None, Kept::Stdout)?;
        Ok(Answer {
            exit: answer.exit.map(|status| session.exit(status, target)),
            ..answer
        })
    }

    /// A machine the inventory names, asked as `dibs --on <machine>` with `flags` would ask it,
    /// within a bound. What stops the call before it is sent is part of what it said.
    pub fn ask(&self, machine: &MachineName, flags: &Call, asked: Asked, bound: Bound) -> Answer {
        let answer = self.asking(machine, flags, asked, bound);
        answer.unwrap_or_else(|e| Answer {
            output: match bound.kept {
                Kept::Stdout | Kept::StdoutAlone => Vec::new(),
                Kept::Everything => e.to_string().into_bytes(),
            },
            exit: Some(e.exit()),
        })
    }

    fn asking(
        &self,
        machine: &MachineName,
        flags: &Call,
        asked: Asked,
        bound: Bound,
    ) -> Result<Answer, CallError> {
        let target = Target::resolve(Some(machine), &TargetEnv::from_env(), &self.fleet)?;
        let values = CallValues {
            tty: false,
            ..self.values_for(flags, asked, &target)?
        };
        let session = Session::new(&target, &self.here);
        let half = MachineHalf::load()?;
        let mut answer = session.ask(&values, &half, bound.within, bound.kept)?;
        if let (Some(status), Kept::Everything) = (answer.exit, bound.kept) {
            let diagnosis = session.diagnose(status, &target);
            answer.output.extend_from_slice(diagnosis.said.as_bytes());
            answer.exit = Some(diagnosis.exit);
        }
        Ok(answer)
    }
}

/// How long a machine asked among several has, and what of its answer is kept.
#[derive(Debug, Clone, Copy)]
pub struct Bound {
    /// None waits for as long as it takes.
    pub within: Option<Duration>,
    pub kept: Kept,
}
