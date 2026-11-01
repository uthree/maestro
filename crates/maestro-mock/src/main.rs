//! `maestro-mock`: a stand-in coding-agent CLI, or an OpenAI-compatible server. See the library.

use std::io::{BufReader, Write};
use std::path::Path;

use maestro_mock::cli::{self, Cli, Invocation};
use maestro_mock::scenario::{Player, Scenario};
use maestro_mock::server::Server;

const USAGE: &str = "usage: maestro-mock (install DIR | serve [--listen ADDRESS])
       or run as claude, codex, agy or opencode (by name, or with their arguments)
the scenario is read from MAESTRO_MOCK_SCENARIO: a JSON file, or JSON itself";

fn main() {
    let program = std::env::args().next().unwrap_or_default();
    let invocation = Invocation::current();
    let args = &invocation.args;
    let scenario = || {
        Scenario::from_env().unwrap_or_else(|error| {
            eprintln!("maestro-mock: {error}");
            std::process::exit(2);
        })
    };
    let cli = Cli::detect(&program, args);
    if args.iter().any(|arg| arg == "--version") {
        let name = cli.map_or("maestro-mock", Cli::executable);
        println!("{} (maestro-mock {})", name, env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Some(cli) = cli {
        let stdin = BufReader::new(std::io::stdin());
        let code = cli::run(cli, &invocation, Player::new(scenario()), stdin, std::io::stdout());
        std::process::exit(code);
    }
    match args.first().map(String::as_str) {
        Some("install") if args.len() == 2 => install(Path::new(&args[1])),
        Some("serve") => {
            let listen = args
                .windows(2)
                .find(|pair| pair[0] == "--listen")
                .map_or("127.0.0.1:8080", |pair| pair[1].as_str());
            let server = Server::start(listen, scenario()).unwrap_or_else(|error| {
                eprintln!("maestro-mock: cannot listen on {listen}: {error}");
                std::process::exit(1);
            });
            println!("{}", server.base_url());
            std::io::stdout().flush().ok();
            loop {
                std::thread::park();
            }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Makes `directory` hold a `claude`, `codex`, `agy` and `opencode` that are this executable.
fn install(directory: &Path) {
    let me = std::env::current_exe().expect("the path of this executable");
    std::fs::create_dir_all(directory).expect("the directory to install into");
    for cli in Cli::ALL {
        let link = directory.join(cli.executable());
        std::fs::remove_file(&link).ok();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&me, &link);
        #[cfg(not(unix))]
        let made = std::fs::copy(&me, link.with_extension("exe")).map(drop);
        if let Err(error) = made {
            eprintln!("maestro-mock: {}: {error}", link.display());
            std::process::exit(1);
        }
        println!("{}", link.display());
    }
}
