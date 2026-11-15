//! Slash commands: reusable prompts in markdown, invoked as `/name args`.
//!
//! Deliberately the same file format Claude Code uses, minus the features Kiln
//! does not implement (see `docs/SKILLS.md`).

use crate::frontmatter;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// `true` when the command came from the project, not `~/.kiln`.
    pub project: bool,
    pub argument_hint: Option<String>,
}

impl Command {
    /// Render the command body with `$ARGUMENTS` and `$1`..`$9` substituted.
    ///
    /// `$ARGUMENTS` is replaced first so a literal `$1` inside the user's own
    /// arguments is not substituted a second time.
    pub fn render(&self, args: &str) -> Result<String, String> {
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("could not read {}: {e}", self.path.display()))?;
        let (_, body) = frontmatter::split(&text);
        let body = body.trim();
        if body.is_empty() {
            return Err(format!("command '{}' has an empty body", self.name));
        }
        if body.contains("$ARGUMENTS") {
            return Ok(body.replace("$ARGUMENTS", args));
        }
        // Positional form: $1 is the first whitespace-separated argument.
        let parts: Vec<&str> = args.split_whitespace().collect();
        let mut out = body.to_string();
        for i in (1..=9).rev() {
            if let Some(value) = parts.get(i - 1) {
                out = out.replace(&format!("${i}"), value);
            }
        }
        // Anything still referring to a missing positional becomes empty rather
        // than leaving a stray `$4` in the prompt.
        for i in 1..=9 {
            out = out.replace(&format!("${i}"), "");
        }
        Ok(out)
    }
}

#[derive(Debug, Default)]
pub struct CommandSet {
    commands: BTreeMap<String, Command>,
}

impl CommandSet {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }

    pub fn catalogue(&self) -> String {
        self.commands
            .values()
            .map(|c| format!("- /{}: {}", c.name, c.description))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn search_dirs(repo_root: &Path) -> Vec<PathBuf> {
    vec![
        repo_root.join(".kiln").join("commands"),
        kiln_core::paths::kiln_dir().join("commands"),
    ]
}

pub fn discover(repo_root: &Path) -> CommandSet {
    let mut set = CommandSet::default();
    for dir in search_dirs(repo_root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let project = dir.starts_with(repo_root);
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (meta, body) = frontmatter::split(&text);
            let name = meta
                .get("name")
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| stem.to_string());
            let description = meta
                .get("description")
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| first_line(&body));
            set.commands.entry(name.clone()).or_insert(Command {
                name,
                description,
                path,
                project,
                argument_hint: meta.get("argument-hint").cloned(),
            });
        }
    }
    set
}

fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("(no description)")
        .chars()
        .take(100)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovers_commands_from_markdown_files() {
        let root = temp("discover");
        let dir = root.join(".kiln").join("commands");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("review.md"),
            "---\ndescription: Review a diff\n---\nReview this: $ARGUMENTS",
        )
        .unwrap();
        std::fs::write(dir.join("plain.md"), "# Title\nJust a prompt.").unwrap();

        let set = discover(&root);
        assert_eq!(set.len(), 2);
        assert_eq!(set.get("review").unwrap().description, "Review a diff");
        assert_eq!(set.get("plain").unwrap().description, "Just a prompt.");
        assert!(set.catalogue().contains("/review: Review a diff"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn renders_arguments_placeholder() {
        let root = temp("args");
        let dir = root.join(".kiln").join("commands");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fix.md");
        std::fs::write(&path, "Fix the failing test: $ARGUMENTS").unwrap();
        let cmd = Command {
            name: "fix".into(),
            description: "d".into(),
            path,
            project: true,
            argument_hint: None,
        };
        assert_eq!(
            cmd.render("auth::login").unwrap(),
            "Fix the failing test: auth::login"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn renders_positional_arguments_and_blanks_the_rest() {
        let root = temp("positional");
        let dir = root.join(".kiln").join("commands");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mv.md");
        std::fs::write(&path, "Move $1 into $2. Ignore $3.").unwrap();
        let cmd = Command {
            name: "mv".into(),
            description: "d".into(),
            path,
            project: true,
            argument_hint: None,
        };
        assert_eq!(
            cmd.render("a.rs b.rs").unwrap(),
            "Move a.rs into b.rs. Ignore ."
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
