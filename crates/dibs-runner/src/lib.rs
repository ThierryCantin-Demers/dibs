//! The machine half of dibs: it takes the lock and runs the job. A client reaches it as
//! `dibs-runner serve` over ssh, or links it and runs it as `dibs __runner serve`, and builds the
//! next version through `dibs-runner build <hash>`; `docs/design/protocol.md` says how.

mod call;
mod channel;
mod clock;
mod gc;
mod history;
mod job;
mod kept;
mod kill;
mod lock;
mod machine;
mod platform;
mod probe;
mod provision;
mod queue;
mod session;
mod settings;
pub mod shared;
mod sink;
mod status;
mod stop;
mod views;

/// What `dibs-runner` does with its arguments, and the exit it gives.
pub fn main(args: &[String]) -> i32 {
    match args {
        [verb] if verb == "serve" => session::serve(),
        [verb, hash] if verb == "build" => provision::build(hash),
        [verb, days, dry] if verb == "gc" => {
            gc::Asked::parse(&format!("{days} {dry}")).sweep().run()
        }
        _ => {
            eprintln!("usage: dibs-runner serve | build <hash> | gc <days|default> <0|1>");
            2
        }
    }
}
