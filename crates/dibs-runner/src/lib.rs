//! The machine half of dibs: it takes the lock and runs the job. A client reaches it as
//! `dibs-runner serve` over ssh, or links it and runs it as `dibs __runner serve`; what passes
//! between them is in `docs/design/protocol.md`.

mod call;
mod channel;
mod clock;
mod history;
mod job;
mod lock;
mod machine;
mod platform;
mod queue;
mod session;
mod settings;
mod sink;
mod stop;

/// What `dibs-runner` does with its arguments, and the exit it gives.
pub fn main(args: &[String]) -> i32 {
    match args {
        [verb] if verb == "serve" => session::serve(),
        _ => {
            eprintln!("usage: dibs-runner serve");
            2
        }
    }
}
