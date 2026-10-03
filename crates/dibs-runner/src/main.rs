use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(dibs_runner::main(&args).rem_euclid(256) as u8)
}
