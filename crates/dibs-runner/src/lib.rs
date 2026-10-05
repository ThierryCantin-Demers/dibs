//! The machine half of dibs: it takes the lock and runs the job. A client reaches it as
//! `dibs-runner serve <hash>` over ssh, or links it and runs it as `dibs __runner serve <hash>`,
//! and builds the next version through `dibs-runner build <hash>`; `docs/design/protocol.md` says
//! how.

mod call;
mod channel;
mod clock;
mod history;
mod itself;
mod job;
mod kept;
mod kill;
mod lock;
mod machine;
mod platform;
mod probe;
mod provision;
mod queue;
mod series;
mod session;
mod settings;
pub mod shared;
mod sink;
mod status;
mod stop;
mod tree;
mod views;

pub use provision::{BUILD_MAX, Source};

/// What `dibs-runner` does with its arguments, and the exit it gives.
pub fn main(args: &[String], source: Source) -> i32 {
    match args {
        [verb, asked] if verb == "serve" => match source.serves(asked) {
            true => session::serve(),
            false => source.refuse(asked),
        },
        [verb] if verb == "hash" => source.name(),
        [verb] if verb == job::Tether::WORD => job::Tether::serve(),
        [verb, hash] if verb == "build" => provision::build(hash),
        [verb, days, dry] if verb == "gc" => {
            tree::Asked::parse(&format!("{days} {dry}")).sweep().run()
        }
        _ => {
            eprintln!(
                "usage: dibs-runner serve <hash> | hash | build <hash> | gc <days|default> <0|1> | tether"
            );
            2
        }
    }
}
