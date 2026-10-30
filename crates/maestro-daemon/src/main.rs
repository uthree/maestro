//! `maestro-daemon`: the daemon alone, for a server that runs no frontend. See
//! [`maestro_daemon::resident`] for its command line.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    maestro_daemon::resident::run(&args)
}
