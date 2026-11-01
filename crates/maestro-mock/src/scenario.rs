//! What the stand-ins answer: rules matched against each prompt, each with the steps of a reply.
//!
//! A scenario is JSON, so a test can write it inline, a person can keep one in a file, and the
//! same file drives every stand-in CLI and the HTTP server alike:
//!
//! ```json
//! {
//!   "stream": {"chunk": 8, "delay_ms": 5},
//!   "rules": [
//!     {"when": {"prompt_contains": "plan", "read_only": true},
//!      "reply": [{"think": "Two steps."}, {"say": "Here is the plan."}]},
//!     {"when": {"prompt_contains": "build"},
//!      "reply": [{"tool": {"name": "Bash", "input": {"command": "cargo build"},
//!                          "output": "Finished", "ask": true}},
//!                {"say": "Built."}]},
//!     {"when": {"prompt_contains": "flaky"}, "times": 1, "reply": [{"fail": "rate limited"}]},
//!     {"when": {"prompt_contains": "slow"}, "reply": [{"sleep_ms": 2000}, {"say": "Late."}]}
//!   ],
//!   "otherwise": [{"say": "You said: {prompt}"}]
//! }
//! ```
//!
//! The first rule whose every condition holds answers; a rule with `times` stops matching once
//! it has answered that many times. Nothing matching, `otherwise` answers, and without it the
//! prompt is echoed.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The variable that names the scenario: a path to a JSON file, or the JSON itself.
pub const SCENARIO_VAR: &str = "MAESTRO_MOCK_SCENARIO";

/// The file a stand-in reads from its working directory when [`SCENARIO_VAR`] is unset, so
/// tests running side by side can each give their own scenario.
pub const SCENARIO_FILE: &str = ".maestro-mock.json";

/// Rules, and how text is streamed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// How text is cut up as it streams.
    #[serde(default)]
    pub stream: Stream,
    /// Tried in order.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// The reply when no rule matches. `None` echoes the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otherwise: Option<Vec<Step>>,
}

/// How [`Step::Say`] and [`Step::Think`] text is streamed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    /// Characters per chunk; 0 sends each text whole.
    #[serde(default)]
    pub chunk: usize,
    /// Pause before each chunk after the first.
    #[serde(default)]
    pub delay_ms: u64,
}

impl Stream {
    /// `text` in chunks.
    pub fn chunks(self, text: &str) -> Vec<String> {
        if self.chunk == 0 || text.is_empty() {
            return vec![text.to_owned()];
        }
        let chars: Vec<char> = text.chars().collect();
        chars.chunks(self.chunk).map(|chunk| chunk.iter().collect()).collect()
    }

    /// The pause between two chunks.
    pub fn delay(self) -> Duration {
        Duration::from_millis(self.delay_ms)
    }
}

/// One rule: when it applies, and what it answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Every condition must hold. Empty matches everything.
    #[serde(default)]
    pub when: When,
    /// How many times the rule may answer, counted per process (per CLI started, or for the
    /// whole life of the server). `None` is unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<u32>,
    /// The steps of the reply, in order.
    pub reply: Vec<Step>,
}

/// Conditions on a prompt. Absent conditions hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// The prompt contains this text. Instructions that a CLI puts in front of the first prompt
    /// count as part of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_contains: Option<String>,
    /// Who is asked: `claude`, `codex`, `agy`, `opencode`, or `openai` for the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The prompt is this turn of the conversation, counting from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    /// The CLI was told to change nothing, as Maestro starts commanders and leads.
    /// Antigravity is never told, so for it this is always false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    /// The model asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One step of a reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Answer text, streamed. `{prompt}` and `{turn}` are replaced by the prompt and its turn.
    Say(String),
    /// Reasoning, streamed. Antigravity has no way to show it, so it drops it.
    Think(String),
    /// A tool call, optionally asking permission first.
    Tool(Tool),
    /// Waits, in milliseconds. An interrupt cuts it short and ends the turn.
    SleepMs(u64),
    /// Waits until the turn is interrupted or input closes.
    Hang,
    /// Ends the turn as failed with this message: an error result from a CLI, or an error
    /// status from the server.
    Fail(String),
    /// The server's error, with its HTTP status. A CLI treats it as [`Step::Fail`].
    HttpError {
        /// The status code, such as 429 or 500.
        status: u16,
        /// The error's message.
        message: String,
    },
    /// Exits the process at once with this code, mid-turn. The server drops the connection.
    Exit(i32),
    /// Writes this line as it is, which need not be JSON, like a CLI's banner or warning.
    Noise(String),
}

/// A tool call the stand-in pretends to make.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    /// The tool's name, as the provider names it (`Bash`, `Edit`, `run_command`, ...).
    pub name: String,
    /// Its arguments. A `command` member makes it a shell command for CLIs that tell those
    /// apart; a `file_path` member makes it a file change.
    #[serde(default)]
    pub input: Value,
    /// What it printed.
    #[serde(default)]
    pub output: String,
    /// Whether it succeeded.
    #[serde(default = "yes")]
    pub ok: bool,
    /// Ask permission first. Refused, the call is reported as failed and the reply goes on.
    #[serde(default)]
    pub ask: bool,
}

fn yes() -> bool {
    true
}

/// What a rule is matched against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt<'a> {
    /// Who is asked, as in [`When::provider`].
    pub provider: &'a str,
    /// The text.
    pub text: &'a str,
    /// The turn, from 1.
    pub turn: u32,
    /// Whether the CLI was told to change nothing.
    pub read_only: bool,
    /// The model asked for.
    pub model: Option<&'a str>,
}

impl When {
    fn holds(&self, prompt: &Prompt) -> bool {
        self.prompt_contains.as_ref().is_none_or(|text| prompt.text.contains(text.as_str()))
            && self.provider.as_ref().is_none_or(|provider| provider == prompt.provider)
            && self.turn.is_none_or(|turn| turn == prompt.turn)
            && self.read_only.is_none_or(|read_only| read_only == prompt.read_only)
            && self.model.as_ref().is_none_or(|model| Some(model.as_str()) == prompt.model)
    }
}

/// A scenario being played: it counts how often each rule has answered.
#[derive(Debug)]
pub struct Player {
    scenario: Scenario,
    used: Vec<u32>,
}

impl Player {
    /// Starts playing `scenario`.
    pub fn new(scenario: Scenario) -> Self {
        let used = vec![0; scenario.rules.len()];
        Self { scenario, used }
    }

    /// How text streams.
    pub fn stream(&self) -> Stream {
        self.scenario.stream
    }

    /// The reply to `prompt`, with `{prompt}` and `{turn}` filled in. Counts towards the
    /// answering rule's `times`.
    pub fn reply(&mut self, prompt: &Prompt) -> Vec<Step> {
        self.choose(prompt, false)
    }

    /// The reply to `prompt` again, as when a client comes back with a tool's result: as
    /// [`Player::reply`], except that a rule used up by its `times` still answers, and nothing
    /// is counted.
    pub fn resume(&mut self, prompt: &Prompt) -> Vec<Step> {
        self.choose(prompt, true)
    }

    fn choose(&mut self, prompt: &Prompt, resuming: bool) -> Vec<Step> {
        let rules = self.scenario.rules.iter().zip(&mut self.used);
        let mut found = rules.filter(|(rule, used)| {
            let left = match rule.times {
                None => true,
                Some(_) if resuming => **used > 0,
                Some(times) => **used < times,
            };
            rule.when.holds(prompt) && left
        });
        let steps = match found.next() {
            Some((rule, used)) => {
                if !resuming {
                    *used += 1;
                }
                rule.reply.clone()
            }
            None => self
                .scenario
                .otherwise
                .clone()
                .unwrap_or_else(|| vec![Step::Say("{prompt}".into())]),
        };
        let fill = |text: String| {
            text.replace("{prompt}", prompt.text).replace("{turn}", &prompt.turn.to_string())
        };
        steps
            .into_iter()
            .map(|step| match step {
                Step::Say(text) => Step::Say(fill(text)),
                Step::Think(text) => Step::Think(fill(text)),
                other => other,
            })
            .collect()
    }
}

impl Scenario {
    /// Reads a scenario from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The scenario named by [`SCENARIO_VAR`]: inline JSON if it starts with `{`, else a path.
    /// Unset, [`SCENARIO_FILE`] in the working directory, and without that every prompt is
    /// echoed.
    pub fn from_env() -> Result<Self, String> {
        let Some(value) = std::env::var_os(SCENARIO_VAR) else {
            return match std::fs::read_to_string(SCENARIO_FILE) {
                Ok(json) => {
                    Self::from_json(&json).map_err(|error| format!("{SCENARIO_FILE}: {error}"))
                }
                Err(_) => Ok(Self::default()),
            };
        };
        let value = value.to_string_lossy();
        let json = if value.trim_start().starts_with('{') {
            value.into_owned()
        } else {
            std::fs::read_to_string(value.as_ref())
                .map_err(|error| format!("{SCENARIO_VAR}={value}: {error}"))?
        };
        Self::from_json(&json).map_err(|error| format!("{SCENARIO_VAR}: {error}"))
    }

    /// The scenario as JSON, to hand to a stand-in through [`SCENARIO_VAR`].
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a scenario is plain data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(text: &str, turn: u32) -> Prompt<'_> {
        Prompt { provider: "claude", text, turn, read_only: false, model: None }
    }

    #[test]
    fn the_first_matching_rule_answers_until_it_is_used_up() {
        let scenario = Scenario::from_json(
            r#"{"rules": [
                {"when": {"prompt_contains": "flaky"}, "times": 1, "reply": [{"fail": "busy"}]},
                {"when": {"prompt_contains": "flaky"}, "reply": [{"say": "ok on turn {turn}"}]},
                {"when": {"provider": "codex"}, "reply": ["hang"]}
            ]}"#,
        )
        .unwrap();
        let mut player = Player::new(scenario);
        assert_eq!(player.reply(&prompt("a flaky one", 1)), [Step::Fail("busy".into())]);
        assert_eq!(player.reply(&prompt("a flaky one", 2)), [Step::Say("ok on turn 2".into())]);
        assert_eq!(player.reply(&prompt("hello", 3)), [Step::Say("hello".into())]);
    }

    #[test]
    fn steps_read_and_write_as_one_key_objects() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"sleep_ms": 5}, {"tool": {"name": "Bash", "input": {"command": "ls"}}},
                {"http_error": {"status": 429, "message": "slow down"}}, {"exit": 3}]"#,
        )
        .unwrap();
        let Step::Tool(tool) = &steps[1] else { panic!() };
        assert!(tool.ok && !tool.ask);
        let again: Vec<Step> =
            serde_json::from_str(&serde_json::to_string(&steps).unwrap()).unwrap();
        assert_eq!(again, steps);
    }

    #[test]
    fn the_demo_scenario_reads() {
        let demo = include_str!("../scenarios/demo.json");
        let scenario = Scenario::from_json(demo).unwrap();
        let mut player = Player::new(scenario);
        let planner = Prompt { read_only: true, ..prompt("Add a greeting", 1) };
        let reply = player.reply(&planner);
        assert!(matches!(&reply[1], Step::Say(text) if text.contains("```maestro-plan")));
    }

    #[test]
    fn text_streams_in_chunks_of_characters() {
        let stream = Stream { chunk: 2, delay_ms: 0 };
        assert_eq!(stream.chunks("日本語です"), ["日本", "語で", "す"]);
        assert_eq!(Stream::default().chunks("whole"), ["whole"]);
    }
}
