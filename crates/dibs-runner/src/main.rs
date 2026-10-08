use dibs_format::wire::SOURCE_HASH;
use dibs_runner::Source;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = Source {
        hash: SOURCE_HASH.unwrap_or_default(),
    };
    ExitCode::from(dibs_runner::main(&args, source).rem_euclid(256) as u8)
}
