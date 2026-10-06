use crate::{
    call::{Journal, ONE_LINE, OneLine as _, Received},
    channel::{Caller, Channel},
    clock::{Moment, Span},
    history::{History, Key, Scope},
    job::{Cap, Environment, Job, Output, Unpinned},
    kept::KeptJobs,
    kill::Kill,
    lock::LockDir,
    machine::Site,
    probe::Probe,
    session::run::{NOT_STARTED, Place},
    settings::Settings,
    sink::Sink,
    status::Look,
    stop::{Signals, Stage, Stopper},
    views::Views,
};
use dibs_format::{
    Event, Exit, Mode,
    wire::{MaxFrom, Record, Request},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// How long a peek's command is given to stop once its cap has passed.
const PEEK_GRACE: Duration = Duration::from_secs(5);
/// History needs this many runs of a job before it may raise the job's cap.
const RUNS_FOR_A_CAP: usize = 3;
/// How much of a command a refusal's log line keeps after the words that say why.
const REFUSED_LINE: usize = 160;

/// One request in, frames out, the exit. A transfer's request is followed by rsync's own stream
/// both ways, so once it says the transfer starts the runner frames nothing, and exits with the
/// call.
pub fn serve() -> i32 {
    let signals = Signals::block();
    let sink = Sink::frames();
    let mut channel = match Channel::stdin() {
        Ok(channel) => channel,
        Err(e) => {
            sink.say(&format!("dibs-runner: stdin could not be read: {e}\n"));
            sink.exit(Exit::Refused.status());
            return Exit::Refused.status();
        }
    };
    let code = match channel.request() {
        Ok(request) if request.mode == Mode::Rsh && request.tree.is_none() => {
            sink.record(Record::Transferring);
            return Visit::new(request, Sink::plain()).serve(Caller::Stdout, signals);
        }
        // Framed until its tree is laid out, which says so before the transfer starts.
        Ok(request) if request.mode == Mode::Rsh => {
            Visit::new(request, sink.clone()).serve(Caller::Stdout, signals)
        }
        Ok(request) => Visit::new(request, sink.clone()).serve(Caller::Channel(channel), signals),
        Err(e) => {
            sink.say(&format!("dibs-runner: {e}\n"));
            Exit::Refused.status()
        }
    };
    sink.exit(code);
    code
}

/// One call on this machine.
pub struct Visit {
    pub call: Received,
    pub sink: Sink,
    pub settings: Settings,
    /// What was made for this call alone, which goes with it however it ends.
    temporary: Vec<PathBuf>,
}

impl Visit {
    pub fn new(request: Request, sink: Sink) -> Visit {
        Visit {
            call: Received::of(request),
            sink,
            settings: Settings::load(),
            temporary: Vec::new(),
        }
    }

    pub fn with_temporary(self, temporary: Vec<PathBuf>) -> Visit {
        Visit { temporary, ..self }
    }

    /// Runs the call to its end; the caller's channel, where it has one, is watched while it is
    /// queued and runs.
    pub fn serve(&self, caller: Caller, signals: Signals) -> i32 {
        if self.call.mode() != Mode::Check {
            for refused in &self.settings.refused {
                self.sink.say(&format!("dibs: {refused}\n"));
            }
        }
        let machine = match Site::set_up() {
            Ok(machine) => machine,
            Err(unwritable) => {
                self.sink.say(&unwritable.said());
                return Exit::NoLock.status();
            }
        };
        let dir = LockDir {
            path: machine.lock_dir.clone(),
        };
        let stopper = Arc::new(
            Stopper::new(
                self.call.clone(),
                dir.clone(),
                machine.log.clone(),
                self.sink.clone(),
            )
            .with_temporary(self.temporary.clone()),
        );
        signals.listen(Arc::clone(&stopper));
        let history = History::load(&machine.history);
        let views = Views {
            look: Look {
                machine: &machine,
                dir: &dir,
                history: &history,
                settings: &self.settings,
                asking: self.call.pid,
            },
            sink: &self.sink,
            request: &self.call.request,
        };
        let kill = || Kill {
            call: &self.call,
            look: views.look,
            sink: &self.sink,
        };
        match self.call.mode() {
            Mode::Status => return views.status(),
            Mode::Watch => return views.watch(caller.channel()),
            Mode::Log => return views.log(),
            Mode::Kill | Mode::KillForce => return kill().serve(),
            Mode::Release => return kill().release(),
            Mode::Check => {
                return Probe {
                    machine: &machine,
                    settings: &self.settings,
                    write: self.call.label().as_str() == "check-write",
                    json: self.call.request.json,
                }
                .serve(&self.sink);
            }
            _ => {}
        }
        let environment = match Environment::of(&machine, self.call.request.card.as_ref()) {
            Ok(environment) => environment,
            Err(Unpinned(said)) => {
                self.sink.say(&said);
                return Exit::Refused.status();
            }
        };
        if let Some(batch) = self.call.batch_id()
            && dir.cancelled(batch)
        {
            self.sink.say(&format!(
                "dibs: batch {batch} was cancelled with dibs --kill, so this step does not run.\n"
            ));
            let mut line = self.call.log_line(Event::Refused);
            line.command = format!(
                "refused, batch cancelled: {}",
                self.call.request.command.one_line(REFUSED_LINE)
            );
            Journal { path: &machine.log }.write(&line);
            return Exit::Cancelled.status();
        }
        let at = Place {
            machine: &machine,
            dir: &dir,
            stopper: &stopper,
        };
        let kept = KeptJobs {
            machine: &machine,
            dir: &dir,
            sink: &self.sink,
            tty: self.call.request.tty,
        };
        match self.call.mode() {
            Mode::Peek => self.peek(&at, &environment, caller),
            Mode::Shared | Mode::Bench | Mode::Rsh | Mode::Gc => self.run(&at, environment, caller),
            Mode::Out => kept.out(self.call.label().as_str()),
            Mode::Fetch => kept.fetch(self.call.label().as_str()),
            mode => {
                self.sink
                    .say(&format!("dibs-runner: no {mode} call is served here\n"));
                Exit::Refused.status()
            }
        }
    }

    /// A peek runs beside whatever is measured, with no lock, and every one is logged: the log has
    /// to say what ran beside which run.
    fn peek(&self, at: &Place, environment: &Environment, caller: Caller) -> i32 {
        let request = &self.call.request;
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
        let job = match Job::spawn(&request.command, environment, Output::Caller, &self.sink) {
            Ok(job) => job,
            Err(e) => {
                self.sink.say(&format!("dibs: bash could not start: {e}\n"));
                return NOT_STARTED;
            }
        };
        state.stage = Stage::Peeking(job.pid);
        drop(state);
        let status = job.wait(cap);
        at.stopper.state().stage = Stage::Setup;
        let took = Moment::epoch_now().saturating_sub(start);
        let journal = Journal {
            path: &at.machine.log,
        };
        let peeked = |event| {
            let mut line = self.call.log_line(event);
            line.ran = Some(took);
            line.exit = Some(status);
            let command = request.command.one_line(ONE_LINE);
            if !command.is_empty() {
                line.command = command;
            }
            line
        };
        journal.write(&peeked(Event::Peek));
        if took >= self.settings.peek_warn {
            self.sink.say(&format!(
                "dibs: that --peek took {} and ran with no lock, beside\n  \
                 whatever is being measured. Anything that costs time belongs in\n  \
                 'dibs <command>', which takes the shared lock.\n",
                Span(took)
            ));
            journal.write(&peeked(Event::PeekSlow));
        }
        status
    }

    /// `--max`, raised to twice what 90% of this job's own runs took when nobody chose it, so work
    /// that always runs long is not killed at its mode's default. Only a shared job or a
    /// benchmark: a transfer and a sweep keep their mode's cap.
    pub fn cap(&self, history: &History) -> u64 {
        let request = &self.call.request;
        let max = request.max;
        let a_job = matches!(self.call.mode(), Mode::Shared | Mode::Bench);
        if !a_job || request.max_from != MaxFrom::Default || request.watch.hold || max == 0 {
            return max;
        }
        let Some(estimate) = history.estimate(Key {
            mode: self.call.mode(),
            label: self.call.label(),
            agent: Some(&self.call.agent),
            fingerprint: request.fingerprint.as_deref(),
        }) else {
            return max;
        };
        if estimate.scope != Scope::This
            || estimate.runs < RUNS_FOR_A_CAP
            || estimate.high * 2 <= max
        {
            return max;
        }
        let raised = estimate.high * 2;
        self.sink.say(&format!(
            "dibs: 90% of {} runs of this took up to {}, so it may hold the lock for {} rather than {}. --max sets it.\n",
            estimate.runs,
            Span(estimate.high),
            Span(raised),
            Span(max)
        ));
        raised
    }

    /// Says what to do about an overrun: run it again, since a compile picks up where it stopped.
    pub fn overran(&self, max: u64) {
        let mut said = format!(
            "dibs: stopped after holding the lock for {max}s, which is --max for a {} job.\n  \
             Nothing is wrong with it; it was simply told to hold no longer than that.\n",
            self.call.mode()
        );
        if !self.call.request.watch.hold {
            said.push_str(
                "  Run it again; a compile picks up from the crates that already finished, since the\n  \
                 build cache outlives the job. Anything else starts over.\n",
            );
        }
        said.push_str(&format!(
            "  If it truly needs one long run, say so:  --max {}\n",
            max * 2
        ));
        self.sink.say(&said);
    }
}
