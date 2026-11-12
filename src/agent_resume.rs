//! Agents come back after their terminal was lost (reboot, core crash).
//!
//! An agent's hook (`herdr-app agent-session`, installed by `herdr-app
//! integrate`) labels its terminal `herdr.resume=<pid> <agent> <session>`:
//! the terminal's process id when the session started, the agent, and its
//! session id or file. A kept terminal that was lost comes back as a shell
//! with a new process id; the app then types the agent's resume command into
//! it. The same process id means the program never went away (the agent was
//! quit or crashed inside its shell), so nothing is typed.

use serde_json::Value;
use std::io::Read;
use std::process::Stdio;

pub const LABEL: &str = "herdr.resume";

const MAX_REF: usize = 4096;

#[derive(Debug, PartialEq)]
pub struct Session {
    pub pid: u64,
    pub agent: String,
    pub reference: String,
}

impl Session {
    pub fn parse(label: &str) -> Option<Session> {
        let mut words = label.splitn(3, ' ');
        let pid = words.next()?.parse().ok()?;
        let agent = words.next()?.to_owned();
        let reference = words.next()?.to_owned();
        command(&agent, &reference)?;
        Some(Session { pid, agent, reference })
    }

    pub fn label(&self) -> String {
        format!("{} {} {}", self.pid, self.agent, self.reference)
    }
}

/// How each agent resumes a session; None for agents Herdr doesn't know or a
/// reference it won't type. Ids are plain words; pi and omp also take a session file.
pub fn command(agent: &str, reference: &str) -> Option<Vec<String>> {
    let r = reference.to_owned();
    let id = valid_id(reference);
    let argv: Vec<String> = match agent {
        "claude" if id => vec!["claude".into(), "--resume".into(), r],
        "codex" if id => vec!["codex".into(), "resume".into(), r],
        "copilot" if id => vec!["copilot".into(), format!("--resume={r}")],
        "cursor" if id => vec![if cfg!(windows) { "cursor-agent.cmd" } else { "cursor-agent" }.into(), "--resume".into(), r],
        "droid" | "grok" | "hermes" | "qwen" if id => vec![agent.into(), "--resume".into(), r],
        "kimi" | "opencode" if id => vec![agent.into(), "--session".into(), r],
        "pi" if id || valid_path(reference) => vec!["pi".into(), "--session".into(), r],
        "omp" if id || valid_path(reference) => vec!["omp".into(), format!("--resume={r}")],
        _ => return None,
    };
    Some(argv)
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && !s.starts_with('-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

/// An absolute path the shells all read the same once quoted.
fn valid_path(s: &str) -> bool {
    let absolute = s.starts_with('/') || s.starts_with("\\\\") || s.as_bytes().get(1..3) == Some(b":\\") || s.as_bytes().get(1..3) == Some(b":/");
    absolute && s.len() <= MAX_REF && !s.chars().any(|c| c.is_control() || matches!(c, '\'' | '"' | '`' | '%' | '\u{2018}'..='\u{201B}'))
}

/// The line to type for `argv` into `shell` (the terminal's program after it
/// came back): words with anything but plain characters are quoted its way.
pub fn shell_line(shell: &str, argv: &[String]) -> String {
    let name = shell.rsplit(['/', '\\']).next().unwrap_or(shell).to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let quote = |w: &String| -> String {
        if !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/:=@+,".contains(&b)) {
            return w.clone();
        }
        match name {
            "cmd" => format!("\"{w}\""),
            _ => format!("'{w}'"),
        }
    };
    argv.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// For a live terminal record: the label to clear and the line to type, when
/// its agent should come back now. Only the app on the terminal's own machine
/// types it (remote addresses are `machine/t7`): two apps watching one core
/// would both type it.
pub fn due(record: &Value) -> Option<(String, String)> {
    if record["address"].as_str().is_none_or(|a| a.contains('/')) {
        return None;
    }
    let label = record.pointer(&format!("/labels/{LABEL}"))?.as_str()?;
    let session = Session::parse(label)?;
    let pid = record["pid"].as_u64()?;
    if record["state"] != "live" || pid == session.pid {
        return None;
    }
    let shell = record.pointer("/argv/0").and_then(Value::as_str).unwrap_or_default();
    let argv = command(&session.agent, &session.reference)?;
    Some((label.to_owned(), shell_line(shell, &argv)))
}

/// What a hook reported.
#[derive(Debug, PartialEq)]
enum Report {
    Started(String),
    /// The person ended the session: don't bring it back.
    Ended,
    Nothing,
}

/// A hook's JSON: Claude Code's own hook input, or the same fields from
/// another agent's hook (`session_path` for a session file).
fn read_report(input: &Value) -> Report {
    // Sub-agents run inside their parent's terminal.
    if input.get("agent_id").is_some_and(|a| !a.is_null()) {
        return Report::Nothing;
    }
    match input["hook_event_name"].as_str() {
        Some("SessionStart") => {
            let reference = input["session_path"].as_str().or(input["session_id"].as_str());
            reference.map_or(Report::Nothing, |r| Report::Started(r.to_owned()))
        }
        // `clear` starts another session at once; a signal (logout, reboot) isn't the person.
        Some("SessionEnd") if matches!(input["reason"].as_str(), Some("prompt_input_exit" | "logout")) => Report::Ended,
        _ => Report::Nothing,
    }
}

/// `herdr-app agent-session <agent>`: a hook's report (JSON on stdin) as
/// this terminal's label. Quiet and successful outside Herdr's terminals.
pub fn report(args: &[String]) -> i32 {
    let Some(agent) = args.first() else {
        eprintln!("usage: herdr-app agent-session <agent> < hook-input.json");
        return 2;
    };
    let mut text = String::new();
    let _ = std::io::stdin().read_to_string(&mut text);
    let Some(terminal) = std::env::var("HC_TERMINAL_ID").ok().filter(|t| !t.is_empty()) else { return 0 };
    let input: Value = serde_json::from_str(&text).unwrap_or_default();
    let value = match read_report(&input) {
        Report::Nothing => return 0,
        Report::Ended => String::new(),
        Report::Started(reference) => {
            let Some(pid) = hc(&["get", &terminal, "--json"]).and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|t| t["pid"].as_u64()) else {
                return 0;
            };
            let session = Session { pid, agent: agent.clone(), reference };
            if Session::parse(&session.label()).is_none() {
                eprintln!("herdr-app: no resume command for {agent} {:?}", session.reference);
                return 0;
            }
            session.label()
        }
    };
    hc(&["label", &terminal, &format!("{LABEL}={value}")]);
    0
}

fn hc(args: &[&str]) -> Option<String> {
    let configured = crate::settings::load_person().ok().and_then(|p| p.core);
    let bin = crate::core_bin::find(configured.as_deref()).unwrap_or_else(|| "hc".into());
    let out = crate::spawn::command(bin).args(args).stdin(Stdio::null()).stderr(Stdio::inherit()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "3f2a9c1e-0b7d-4e55-9a10-2c8f6d4b7e01";

    #[test]
    fn labels_round_trip_and_unknown_agents_are_refused() {
        let s = Session { pid: 42, agent: "pi".into(), reference: "/home/a b/.pi/s.jsonl".into() };
        assert_eq!(Session::parse(&s.label()), Some(s));
        assert_eq!(Session::parse(&format!("42 vim {ID}")), None);
        assert_eq!(Session::parse(&format!("x claude {ID}")), None);
        assert_eq!(Session::parse("42 claude"), None);
    }

    #[test]
    fn references_that_could_inject_are_refused() {
        for bad in ["a;rm -rf ~", "-x", "a b", "", "a\nb", "$(id)"] {
            assert_eq!(command("claude", bad), None, "{bad:?}");
        }
        for bad in ["relative/s.jsonl", "/a'b", "/a\"b", "/a\nb", "/a`b"] {
            assert_eq!(command("pi", bad), None, "{bad:?}");
        }
        assert_eq!(command("claude", "/abs/path"), None, "claude takes ids only");
        assert!(command("pi", "C:\\Users\\a b\\s.jsonl").is_some());
    }

    #[test]
    fn lines_quote_for_the_shell_that_came_back() {
        let argv = command("pi", "/home/a b/s.jsonl").unwrap();
        assert_eq!(shell_line("/usr/bin/bash", &argv), "pi --session '/home/a b/s.jsonl'");
        assert_eq!(shell_line("C:\\Windows\\System32\\cmd.exe", &argv), "pi --session \"/home/a b/s.jsonl\"");
        assert_eq!(shell_line("pwsh.exe", &argv), "pi --session '/home/a b/s.jsonl'");
        assert_eq!(shell_line("fish", &command("claude", ID).unwrap()), format!("claude --resume {ID}"));
    }

    #[test]
    fn only_a_replaced_program_resumes() {
        let rec = |pid: Value, label: &str| json!({ "address": "t1", "state": "live", "pid": pid, "argv": ["/bin/zsh"], "labels": { LABEL: label } });
        let label = format!("100 claude {ID}");
        assert_eq!(due(&rec(json!(100), &label)), None, "same program: quit or crashed inside its shell");
        assert_eq!(due(&rec(Value::Null, &label)), None, "lost, not back yet");
        assert_eq!(due(&rec(json!(200), &label)), Some((label.clone(), format!("claude --resume {ID}"))));
        assert_eq!(due(&rec(json!(200), "")), None, "ended by the person");
        assert_eq!(due(&json!({ "address": "t1", "state": "live", "pid": 200, "labels": {} })), None);
        let mut remote = rec(json!(200), &label);
        remote["address"] = "vps/t1".into();
        assert_eq!(due(&remote), None, "the app on that machine resumes it");
    }

    #[test]
    fn hooks_report_starts_and_deliberate_ends() {
        let start = json!({ "hook_event_name": "SessionStart", "session_id": ID, "source": "startup" });
        assert_eq!(read_report(&start), Report::Started(ID.into()));
        let pi = json!({ "hook_event_name": "SessionStart", "session_id": "x", "session_path": "/s.jsonl" });
        assert_eq!(read_report(&pi), Report::Started("/s.jsonl".into()), "a session file wins");
        let sub = json!({ "hook_event_name": "SessionStart", "session_id": ID, "agent_id": "a1" });
        assert_eq!(read_report(&sub), Report::Nothing);
        for (reason, report) in [("prompt_input_exit", Report::Ended), ("logout", Report::Ended), ("clear", Report::Nothing), ("other", Report::Nothing)] {
            assert_eq!(read_report(&json!({ "hook_event_name": "SessionEnd", "reason": reason })), report, "{reason}");
        }
    }
}
