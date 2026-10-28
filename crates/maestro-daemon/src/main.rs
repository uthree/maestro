//! `maestro-daemon --stdio`: serves one frontend over standard input and output.
//!
//! This is what a frontend runs, directly or as `ssh host maestro-daemon --stdio`, so the
//! daemon needs no open port of its own and SSH does the authentication. Diagnostics go to
//! standard error, which `ssh` passes back to the frontend separately from the protocol.

use std::io;
use std::process::ExitCode;

use maestro_daemon::Hub;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args != ["--stdio"] {
        eprintln!("usage: maestro-daemon --stdio");
        return ExitCode::from(2);
    }
    match Hub::new().serve(io::stdin(), io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("maestro-daemon: {error}");
            ExitCode::FAILURE
        }
    }
}
