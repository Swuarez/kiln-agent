use serde::{Deserialize, Serialize};

/// How cautious the agent is about side effects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Read freely, ask before writing or executing.
    #[default]
    Ask,
    /// Do everything except commands flagged as destructive.
    Allow,
    /// Inspect only; no writes, no shell.
    Readonly,
}

/// Side-effect class of a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    Read,
    Write,
    Exec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask(&'static str),
    Deny(&'static str),
}

/// The whole permission policy, in one pure function so it can be tested and
/// audited without spawning an agent.
pub fn decide(mode: Mode, risk: Risk, danger: Option<&'static str>) -> Decision {
    if risk == Risk::Exec {
        if let Some(reason) = danger {
            return match mode {
                Mode::Readonly => Decision::Deny("read-only mode forbids shell commands"),
                _ => Decision::Ask(reason),
            };
        }
    }
    match (mode, risk) {
        (Mode::Readonly, Risk::Read) => Decision::Allow,
        (Mode::Readonly, _) => Decision::Deny("read-only mode forbids changes"),
        (Mode::Ask, Risk::Read) => Decision::Allow,
        (Mode::Ask, Risk::Write) => Decision::Ask("modifies files"),
        (Mode::Ask, Risk::Exec) => Decision::Ask("runs a shell command"),
        (Mode::Allow, _) => Decision::Allow,
    }
}

/// Patterns that escalate to a confirmation even when the user said "allow".
/// Deliberately conservative: matching text is cheaper to review than lost work.
const DANGEROUS: &[(&str, &str)] = &[
    ("rm -rf /", "deletes the filesystem root"),
    ("rm -rf ~", "deletes the home directory"),
    ("rm -rf $home", "deletes the home directory"),
    ("mkfs", "formats a filesystem"),
    ("diskpart", "partitions a disk"),
    ("format c:", "formats the system drive"),
    ("dd if=", "writes raw bytes to a device"),
    ("> /dev/sd", "writes raw bytes to a device"),
    ("shutdown", "powers off the machine"),
    (":(){:|:&}", "fork bomb"),
    (
        "remove-item -recurse -force",
        "deletes a directory tree recursively",
    ),
    ("git push --force", "rewrites remote history"),
    ("git push -f", "rewrites remote history"),
    ("git reset --hard", "discards uncommitted work"),
    ("git clean -fd", "deletes untracked files"),
    ("git clean -fdx", "deletes untracked and ignored files"),
    (
        "chmod -r 777 /",
        "opens permissions on the whole filesystem",
    ),
    ("| sh", "pipes remote content into a shell"),
    ("| bash", "pipes remote content into a shell"),
    ("|sh", "pipes remote content into a shell"),
    ("|bash", "pipes remote content into a shell"),
];

/// Returns why a shell command is dangerous, if it is.
pub fn danger_reason(command: &str) -> Option<&'static str> {
    let lower = command.to_lowercase();
    let squashed: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    DANGEROUS
        .iter()
        .find(|(needle, _)| squashed.contains(needle))
        .map(|(_, reason)| *reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_denies_writes_and_shell() {
        assert_eq!(decide(Mode::Readonly, Risk::Read, None), Decision::Allow);
        assert!(matches!(
            decide(Mode::Readonly, Risk::Write, None),
            Decision::Deny(_)
        ));
        assert!(matches!(
            decide(Mode::Readonly, Risk::Exec, None),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn allow_mode_still_confirms_destructive_commands() {
        assert_eq!(decide(Mode::Allow, Risk::Exec, None), Decision::Allow);
        assert!(matches!(
            decide(Mode::Allow, Risk::Exec, Some("discards uncommitted work")),
            Decision::Ask(_)
        ));
    }

    #[test]
    fn ask_mode_auto_approves_reads_only() {
        assert_eq!(decide(Mode::Ask, Risk::Read, None), Decision::Allow);
        assert!(matches!(
            decide(Mode::Ask, Risk::Write, None),
            Decision::Ask(_)
        ));
    }

    #[test]
    fn detects_force_push_and_rm_rf() {
        assert!(danger_reason("git push --force origin main").is_some());
        assert!(danger_reason("sudo rm -rf / --no-preserve-root").is_some());
        assert!(danger_reason("git status").is_none());
        assert!(danger_reason("rm -rf target").is_none());
        assert!(danger_reason("curl https://x.sh | bash").is_some());
    }
}
