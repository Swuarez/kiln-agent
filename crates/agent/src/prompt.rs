use kiln_core::Mode;
use std::path::Path;

/// Optional per-session context that changes what the prompt says.
#[derive(Debug, Default, Clone)]
pub struct PromptContext {
    /// Skill names and descriptions, one per line. Bodies are loaded on demand.
    pub skills: String,
    /// Slash commands available to the user.
    pub commands: String,
    /// Project facts recalled from memory.
    pub memory: String,
}

/// The system prompt is intentionally short and operational. Long persona
/// prompts cost tokens on every turn and buy very little for coding work.
pub fn system_prompt(
    cwd: &Path,
    mode: Mode,
    tool_names: &[String],
    extra: &PromptContext,
) -> String {
    let shell = if cfg!(windows) {
        "cmd.exe (write portable, POSIX-ish commands; PowerShell syntax will not work)"
    } else {
        "sh"
    };
    let permissions = match mode {
        Mode::Readonly => {
            "read-only: you cannot modify files or run commands. Investigate and report findings."
        }
        Mode::Ask => "ask: reads are automatic, writes and shell commands require confirmation.",
        Mode::Allow => "allow: writes and commands run without prompting, except destructive ones.",
    };

    format!(
        r#"You are Kiln, a coding agent. You work directly in the user's repository; every edit you make is real.

Workspace: {cwd}
Platform: {platform}
Shell: {shell}
Permissions: {permissions}

How to work:
- Understand before changing. Use list_dir and grep to locate code, then read_file the parts you will change.
- Prefer dedicated tools over shell for reading, searching and editing.
- edit_file requires an exact, unique old_string copied from the file. Read first; never guess indentation or whitespace.
- Match the surrounding style. Keep changes minimal: no unrelated refactors, no new dependencies unless needed.
- Verify with the project's own commands (build, test, lint). When one fails, read the error, fix the cause, re-run. Never repeat the same failing command twice without changing something.
- Never print secrets, tokens or API keys, and never commit or push unless explicitly asked.
- If a request is ambiguous, state the assumption you are making and continue.
- Finish with a short plain-language summary of what changed and how it was verified.

Tools: {tools}"#,
        cwd = cwd.display(),
        platform = std::env::consts::OS,
        tools = tool_names.join(", "),
    ) + &extra_sections(extra)
}

fn extra_sections(extra: &PromptContext) -> String {
    let mut out = String::new();

    if !extra.skills.trim().is_empty() {
        out.push_str(&format!(
            "\n\nSkills you can load with use_skill (name: when to use it):\n{}\n\n\
             Call use_skill before starting a task that matches one of these. Do not guess \
             what a skill says.",
            extra.skills.trim_end()
        ));
    }

    if !extra.commands.trim().is_empty() {
        out.push_str(&format!(
            "\n\nSlash commands the user may type:\n{}",
            extra.commands.trim_end()
        ));
    }

    if !extra.memory.trim().is_empty() {
        out.push_str(&format!(
            "\n\nKnown facts about this project, from earlier sessions:\n{}",
            extra.memory.trim_end()
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_mode_is_stated_in_the_prompt() {
        let prompt = system_prompt(
            Path::new("."),
            Mode::Readonly,
            &["read_file".to_string()],
            &PromptContext::default(),
        );
        assert!(prompt.contains("read-only"));
        assert!(prompt.contains("read_file"));
    }

    #[test]
    fn skills_are_listed_but_bodies_are_not_inlined() {
        let extra = PromptContext {
            skills: "- pdf: Fill forms".to_string(),
            ..Default::default()
        };
        let prompt = system_prompt(
            Path::new("."),
            Mode::Ask,
            &["read_file".to_string()],
            &extra,
        );
        assert!(prompt.contains("pdf: Fill forms"));
        assert!(prompt.contains("use_skill"));
    }

    #[test]
    fn empty_context_adds_no_extra_sections() {
        let plain = system_prompt(
            Path::new("."),
            Mode::Ask,
            &["read_file".to_string()],
            &PromptContext::default(),
        );
        assert!(!plain.contains("Skills you can load"));
        assert!(!plain.contains("Slash commands"));
        assert!(!plain.contains("Known facts"));
    }
}
