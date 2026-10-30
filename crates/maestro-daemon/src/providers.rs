//! The coding-agent CLIs on each host: which are installed, and installing or updating them.
//!
//! What a host has is looked up once and kept, because the router asks for every node it starts;
//! [`Command::ListProviders`] looks again, and so does the end of every install or update. An
//! install or update runs its provider's documented script ([`ProviderKind::setup_script`]) on
//! its own thread and reports each line it writes as an [`Event::ProviderSetup`], so every
//! frontend can follow it and the connection that asked stays free.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;

use maestro_core::{Host, ProviderKind, ProviderStatus, SetupAction};
use maestro_host::HostCommand;
use maestro_protocol::{Command, CommandError, Event, Reply, SetupProgress};

use crate::runtime::Broadcast;

pub(crate) struct Providers {
    /// What each host had when last looked at.
    found: Mutex<HashMap<Host, Vec<ProviderStatus>>>,
    /// The installs and updates running now.
    running: Mutex<HashMap<(Host, ProviderKind), SetupAction>>,
    broadcast: Broadcast,
}

impl Providers {
    pub(crate) fn new(broadcast: Broadcast) -> Arc<Self> {
        Arc::new(Self { found: Mutex::default(), running: Mutex::default(), broadcast })
    }

    /// Whether `command` belongs here.
    pub(crate) fn handles(command: &Command) -> bool {
        matches!(command, Command::ListProviders { .. } | Command::SetupProvider { .. })
    }

    pub(crate) fn execute(self: &Arc<Self>, command: Command) -> Result<Reply, CommandError> {
        match command {
            Command::ListProviders { host } => Ok(Reply::Providers { providers: self.scan(&host) }),
            Command::SetupProvider { host, provider, action } => self.setup(host, provider, action),
            other => unreachable!("not a provider command: {other:?}"),
        }
    }

    /// The CLIs installed on `host`, looked up the first time it is asked about.
    pub(crate) fn installed(&self, host: &Host) -> Vec<ProviderKind> {
        let known = self.found.lock().expect("providers lock").get(host).cloned();
        known
            .unwrap_or_else(|| self.scan(host))
            .into_iter()
            .filter(ProviderStatus::installed)
            .map(|status| status.provider)
            .collect()
    }

    /// Looks for every CLI on `host` and keeps what it found.
    fn scan(&self, host: &Host) -> Vec<ProviderStatus> {
        let names = ProviderKind::ALL.map(ProviderKind::executable).join(" ");
        // One round trip for all of them: each found CLI prints its name, path and version.
        let script = format!(
            "for name in {names}; do path=$(command -v \"$name\") && \
             printf '%s\\t%s\\t%s\\n' \"$name\" \"$path\" \
             \"$(\"$name\" --version 2>/dev/null </dev/null | head -n 1)\"; done; true"
        );
        let found = HostCommand::new("sh").args(["-c", &script]).run(host).unwrap_or_default();
        let running = self.running.lock().expect("providers lock").clone();
        let statuses: Vec<ProviderStatus> = ProviderKind::ALL
            .into_iter()
            .map(|provider| {
                let line = found.lines().find_map(|line| {
                    let mut fields = line.splitn(3, '\t');
                    (fields.next() == Some(provider.executable()))
                        .then(|| (fields.next().unwrap_or_default(), fields.next()))
                });
                ProviderStatus {
                    provider,
                    path: line.map(|(path, _)| path.to_owned()),
                    version: line
                        .and_then(|(_, version)| version)
                        .map(str::trim)
                        .filter(|version| !version.is_empty())
                        .map(str::to_owned),
                    setup: running.get(&(host.clone(), provider)).copied(),
                }
            })
            .collect();
        self.found.lock().expect("providers lock").insert(host.clone(), statuses.clone());
        statuses
    }

    /// Starts installing or updating `provider` on `host`, refusing when one is already running.
    fn setup(
        self: &Arc<Self>,
        host: Host,
        provider: ProviderKind,
        action: SetupAction,
    ) -> Result<Reply, CommandError> {
        let mut running = self.running.lock().expect("providers lock");
        if running.contains_key(&(host.clone(), provider)) {
            return Err(CommandError::Host {
                message: format!("{} is already being installed or updated", provider.label()),
            });
        }
        running.insert((host.clone(), provider), action);
        drop(running);
        let script = provider.setup_script(action);
        let progress = |progress| Event::ProviderSetup { host: host.clone(), provider, progress };
        (self.broadcast)(progress(SetupProgress::Started { action, script: script.into() }));

        // Both streams in one, so the lines arrive in the order the script wrote them.
        let mut process = HostCommand::new("sh")
            .args(["-c", &format!("{{ {script}\n}} 2>&1")])
            .process(&host)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let this = Arc::clone(self);
        thread::spawn(move || {
            let progress =
                |progress| Event::ProviderSetup { host: host.clone(), provider, progress };
            let ok = match &mut process {
                Ok(child) => {
                    if let Some(stdout) = child.stdout.take() {
                        for line in BufReader::new(stdout).lines() {
                            let Ok(line) = line else { break };
                            (this.broadcast)(progress(SetupProgress::Output { line }));
                        }
                    }
                    child.wait().is_ok_and(|status| status.success())
                }
                Err(error) => {
                    let line = format!("could not start sh: {error}");
                    (this.broadcast)(progress(SetupProgress::Output { line }));
                    false
                }
            };
            this.running.lock().expect("providers lock").remove(&(host.clone(), provider));
            let status = this
                .scan(&host)
                .into_iter()
                .find(|status| status.provider == provider)
                .expect("every provider is scanned");
            (this.broadcast)(progress(SetupProgress::Finished { ok, status }));
        });
        Ok(Reply::Done)
    }
}
