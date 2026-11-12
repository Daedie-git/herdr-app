//! `herdr-app integrate <agent> [--remove]`: hooks that tell Herdr an agent's
//! session (`herdr-app agent-session`), so it comes back after its terminal
//! was lost (agent_resume.rs). Claude Code: two hooks in its settings.json.
//! pi: an extension file.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const PI_EXTENSION: &str = include_str!("integrate/pi.ts");
const PI_FILE: &str = "herdr-app-resume.ts";
const CLAUDE_EVENTS: [&str; 2] = ["SessionStart", "SessionEnd"];

pub fn main(args: &[String]) -> i32 {
    let remove = args.iter().any(|a| a == "--remove");
    let agent = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("herdr-app: cannot find this program: {e}");
            return 1;
        }
    };
    let home = crate::mods::home();
    let result = match agent {
        Some("claude") => claude(&claude_settings(&home), &exe, remove),
        Some("pi") => pi(&pi_extensions(&home), &exe, remove),
        _ => {
            eprintln!("usage: herdr-app integrate <claude|pi> [--remove]");
            return 2;
        }
    };
    match result {
        Ok(path) => {
            println!("{} {}", if remove { "removed from" } else { "wrote" }, path.display());
            0
        }
        Err(e) => {
            eprintln!("herdr-app: {e}");
            1
        }
    }
}

fn claude_settings(home: &Path) -> PathBuf {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()).map_or_else(|| home.join(".claude"), PathBuf::from);
    dir.join("settings.json")
}

fn pi_extensions(home: &Path) -> PathBuf {
    let dir = std::env::var_os("PI_CODING_AGENT_DIR").filter(|d| !d.is_empty()).map_or_else(|| home.join(".pi/agent"), PathBuf::from);
    dir.join("extensions")
}

/// The hook's command line. Claude Code runs hooks in a POSIX shell (Git
/// Bash on Windows): Windows paths go with slashes, all in single quotes.
fn hook_command(exe: &Path, agent: &str) -> String {
    let mut path = exe.to_string_lossy().into_owned();
    if cfg!(windows) {
        path = path.replace('\\', "/");
    }
    format!("'{}' agent-session {agent}", path.replace('\'', "'\\''"))
}

fn ours(hook: &Value) -> bool {
    hook["command"].as_str().is_some_and(|c| c.ends_with(" agent-session claude"))
}

/// Settings with our hooks replaced (or only removed); everything else as it was.
fn claude_hooks(mut settings: Value, command: Option<&str>) -> Result<Value, String> {
    let root = settings.as_object_mut().ok_or("settings.json is not a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("settings.json: \"hooks\" is not an object")?;
    for event in CLAUDE_EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let list = groups.as_array_mut().ok_or(format!("settings.json: hooks.{event} is not a list"))?;
        for group in list.iter_mut() {
            if let Some(inner) = group["hooks"].as_array_mut() {
                inner.retain(|h| !ours(h));
            }
        }
        list.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
        if let Some(command) = command {
            list.push(json!({ "hooks": [{ "type": "command", "command": command, "timeout": 10 }] }));
        }
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(settings)
}

fn claude(path: &Path, exe: &Path, remove: bool) -> Result<PathBuf, String> {
    let settings = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let command = hook_command(exe, "claude");
    let settings = claude_hooks(settings, (!remove).then_some(command.as_str()))?;
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())? + "\n";
    write(path, &text)?;
    Ok(path.to_owned())
}

fn pi(dir: &Path, exe: &Path, remove: bool) -> Result<PathBuf, String> {
    let path = dir.join(PI_FILE);
    if remove {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", path.display())),
            _ => return Ok(path),
        }
    }
    let exe = serde_json::to_string(&exe.to_string_lossy()).map_err(|e| e.to_string())?;
    write(&path, &PI_EXTENSION.replace("\"__HERDR_APP__\"", &exe))?;
    Ok(path)
}

/// Whole or not at all: an agent may read the file while it changes. A link
/// (dotfiles) keeps pointing at its file, which keeps its permissions
/// (settings can hold keys); the new copy is private until then.
fn write(path: &Path, text: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = path.with_extension("herdr-tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp).map_err(fail)?, text.as_bytes()).map_err(fail)?;
    if let Ok(meta) = std::fs::metadata(&path) {
        std::fs::set_permissions(&tmp, meta.permissions()).map_err(fail)?;
    }
    std::fs::rename(&tmp, &path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_hooks_are_added_once_and_removed_cleanly() {
        let mine = json!({ "matcher": "startup", "hooks": [{ "type": "command", "command": "mine.sh" }] });
        let before = json!({ "model": "opus", "hooks": { "SessionStart": [mine.clone()] }, "theme": "dark" });
        let cmd = "'/opt/herdr app/herdr-app' agent-session claude";
        let once = claude_hooks(before.clone(), Some(cmd)).unwrap();
        let twice = claude_hooks(once.clone(), Some(cmd)).unwrap();
        assert_eq!(once, twice, "installing again changes nothing");
        assert_eq!(once["hooks"]["SessionStart"][0], mine, "other hooks stay");
        assert_eq!(once["hooks"]["SessionStart"][1]["hooks"][0]["command"], cmd);
        assert_eq!(once["hooks"]["SessionEnd"][0]["hooks"][0]["command"], cmd);
        let keys: Vec<&String> = once.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "theme"], "the file keeps its order");
        assert_eq!(claude_hooks(once, None).unwrap(), before);
        assert_eq!(claude_hooks(json!({}), None).unwrap(), json!({}));
        assert!(claude_hooks(json!([]), Some(cmd)).is_err());
    }

    #[test]
    fn hook_commands_survive_the_hook_shell() {
        let exe = if cfg!(windows) { Path::new("C:\\Users\\$(x) it's\\herdr-app.exe") } else { Path::new("/home/$(x) it's/herdr-app") };
        let expected = if cfg!(windows) { "'C:/Users/$(x) it'\\''s/herdr-app.exe' agent-session claude" } else { "'/home/$(x) it'\\''s/herdr-app' agent-session claude" };
        assert_eq!(hook_command(exe, "claude"), expected);
        assert!(ours(&json!({ "command": expected })));
    }

    #[cfg(unix)]
    #[test]
    fn settings_keep_their_link_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_tmp().join(format!("herdr-integrate-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
        let (target, link) = (dir.join("dotfiles/settings.json"), dir.join("settings.json"));
        std::fs::write(&target, r#"{"env":{"KEY":"secret"}}"#).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        claude(&link, Path::new("/x/herdr-app"), false).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
        assert!(std::fs::read_to_string(&target).unwrap().contains("agent-session claude"), "its file has the hooks");
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_pi_extension_names_this_program() {
        let dir = crate::test_tmp().join(format!("herdr-integrate-{}", std::process::id()));
        let path = pi(&dir, Path::new("/x/herdr \"app\""), false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#"const HERDR_APP = "/x/herdr \"app\"";"#), "{text}");
        pi(&dir, Path::new("/x"), true).unwrap();
        assert!(!path.exists());
        pi(&dir, Path::new("/x"), true).expect("removing twice is fine");
        let _ = std::fs::remove_dir_all(dir);
    }
}
