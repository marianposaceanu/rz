use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match rz::run(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            rz::App::print_error(&error);
            ExitCode::FAILURE
        }
    }
}
