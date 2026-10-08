use crate::{
    call::{Journal, ONE_LINE, OneLine as _},
    channel::Caller,
    clock::{Moment, Span},
    job::{Cap, Environment, Job, Output},
    session::run::{NOT_STARTED, Serving},
    stop::Stage,
};
use dibs_format::Event;
use std::{sync::Arc, time::Duration};

/// How long a peek's command is given to stop once its cap has passed.
const PEEK_GRACE: Duration = Duration::from_secs(5);

/// A look at the machine: run beside whatever is measured, with no lock.
pub struct Peek<'a> {
    at: Serving<'a>,
}

impl<'a> Peek<'a> {
    pub fn new(at: Serving<'a>) -> Self {
        Peek { at }
    }

    /// A peek runs beside whatever is measured, with no lock, and every one is logged: the log has
    /// to say what ran beside which run.
    pub fn serve(&self, environment: &Environment, caller: Caller) -> i32 {
        let at = &self.at;
        let request = &at.call.request;
        // A command that writes nothing would otherwise outlive its caller to its cap.
        if let Caller::Channel(channel) = caller
            && !request.watch.off
        {
            channel.watch(request.watch.lease, Arc::clone(at.stopper), None);
        }
        let start = Moment::epoch_now();
        let cap = (request.max > 0).then(|| Cap {
            after: Duration::from_secs(request.max),
            grace: PEEK_GRACE,
        });
        let mut state = at.stopper.state();
        let job = match Job::spawn(&request.command, environment, Output::Caller, at.sink, None) {
            Ok(job) => job,
            Err(e) => {
                at.sink.say(&format!("dibs: bash could not start: {e}\n"));
                return NOT_STARTED;
            }
        };
        state.stage = Stage::Peeking(job.pid);
        drop(state);
        let status = job.wait(cap).status;
        at.stopper.state().stage = Stage::Setup;
        let took = Moment::epoch_now().saturating_sub(start);
        let journal = Journal {
            path: &at.machine.log,
        };
        let peeked = |event| {
            let mut line = at.call.log_line(event);
            line.ran = Some(took);
            line.exit = Some(status);
            let command = request.command.one_line(ONE_LINE);
            if !command.is_empty() {
                line.command = command;
            }
            line
        };
        journal.write(&peeked(Event::Peek));
        if took >= at.settings.peek_warn {
            at.sink.say(&format!(
                "dibs: that --peek took {} and ran with no lock, beside\n  \
                 whatever is being measured. Anything that costs time belongs in\n  \
                 'dibs <command>', which takes the shared lock.\n",
                Span(took)
            ));
            journal.write(&peeked(Event::PeekSlow));
        }
        status
    }
}
