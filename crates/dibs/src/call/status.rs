use crate::{
    call::{
        base::CallError,
        machine::{Asked, Bound, MachineCall},
    },
    cli::Call,
    machine::{Answer, Kept},
    render::{Answered, Answers},
};
use dibs_format::{Exit, MachineName, Mode};

/// How long `dibs status` waits for each machine when it asks them all.
const STATUS_POLL_SECS: u64 = 8;

impl MachineCall<'_> {
    /// One machine's status, or every machine's when the call names none.
    pub fn status(&self) -> Result<i32, CallError> {
        let target = self.target()?;
        let everywhere =
            self.call.all || (target.host.is_empty() && !self.here.local && self.fleet.exists());
        if !everywhere {
            self.somewhere(&target)?;
            return self.send(Asked::plain(Mode::Status, self.label()), &target);
        }
        if !self.fleet.exists() {
            eprintln!("no inventory at {}", self.fleet.shown());
            return Ok(i32::from(Exit::Refused.code()));
        }
        let names = self.fleet.names();
        let flags = Call {
            json: self.call.json,
            ..Call::default()
        };
        let bound = Bound::polled(STATUS_POLL_SECS, Kept::Everything);
        let answers = self.each(&names, |machine| {
            self.ask(
                machine,
                &flags,
                Asked::plain(Mode::Status, self.label()),
                bound,
            )
        });
        print!(
            "{}",
            Answers {
                answers: &answers,
                spaced: true,
            }
        );
        Ok(0)
    }

    /// Asks each machine at once, and gives their answers in the order named.
    pub fn each(
        &self,
        names: &[MachineName],
        ask: impl Fn(&MachineName) -> Answer + Sync,
    ) -> Vec<Answered> {
        std::thread::scope(|scope| {
            let asking: Vec<_> = names
                .iter()
                .map(|name| (name, scope.spawn(|| ask(name))))
                .collect();
            asking
                .into_iter()
                .map(|(name, asked)| match asked.join() {
                    Ok(answer) => Answered {
                        machine: name.clone(),
                        answer,
                    },
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        })
    }
}
