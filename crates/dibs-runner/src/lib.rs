//! The machine half of dibs: it takes the lock and runs the job. A client reaches it as
//! `dibs-runner serve` over ssh, or links it and runs it as `dibs __runner serve`, and builds the
//! next version through `dibs-runner build <hash>`; `docs/design/protocol.md` says how.

mod call;
mod channel;
mod clock;
mod history;
mod job;
mod lock;
mod machine;
mod platform;
mod provision;
mod queue;
mod session;
mod settings;
mod sink;
mod stop;

/// What `dibs-runner` does with its arguments, and the exit it gives.
pub fn main(args: &[String]) -> i32 {
    match args {
        [verb] if verb == "serve" => session::serve(),
        [verb, hash] if verb == "build" => provision::build(hash),
        _ => {
            eprintln!("usage: dibs-runner serve | build <hash>");
            2
        }
    }
}
