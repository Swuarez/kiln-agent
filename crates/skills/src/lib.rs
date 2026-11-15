//! Skills: markdown instructions the model can pull in when relevant.
//!
//! The point of a skill is that it costs almost nothing until it is used. Only
//! the name and description go into the prompt; the body is read from disk when
//! the model calls `use_skill`. This mirrors how Claude Code skills behave, so
//! existing skill directories work without edits.

pub mod commands;
pub mod frontmatter;
pub mod tool;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where skills are looked for, in priority order. The first definition of a
/// name wins, so a project can shadow a global skill of the same name.
pub fn search_dirs(repo_root: &Path) -> Vec<PathBuf> {
    vec![
        repo_root.join(".kiln").join("skills"),
        kiln_core::paths::kiln_dir().join("skills"),
    ]
}

#[derive(Debug, Clone)]
pub struct Skill {
    /// Stable identifier: the directory name, or the `name:` field if present.
    pub name: String,
    pub description: String,
    /// Full markdown body, loaded lazily.
    pub path: PathBuf,
    pub source: SkillSource,
    /// Frontmatter `allowed-tools`, kept for reporting. Kiln does not restrict
    /// tools per skill yet; this is surfaced by `kiln skills` so the gap is
    /// visible rather than silently ignored.
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    Project,
    Global,
}

impl Skill {
    /// Reads the body from disk on demand — this is the expensive half that
    /// progressive disclosure exists to avoid.
    pub fn body(&self) -> std::io::Result<String> {
        let text = std::fs::read_to_string(&self.path)?;
        Ok(frontmatter::split(&text).1.trim().to_string())
    }
}

#[derive(Debug, Default)]
pub struct SkillSet {
    skills: BTreeMap<String, Skill>,
}

impl SkillSet {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    /// The compact catalogue that goes into the system prompt: name plus
    /// description, nothing else.
    pub fn catalogue(&self) -> String {
        self.skills
            .values()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Load the body for a skill, reporting an error string suitable for a tool
    /// result rather than an io::Error the model cannot act on.
    pub fn load_body(&self, name: &str) -> Result<String, String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| format!("unknown skill '{name}'"))?;
        let body = skill
            .body()
            .map_err(|e| format!("could not read {}: {e}", skill.path.display()))?;
        if body.is_empty() {
            return Err(format!("skill '{name}' has an empty body"));
        }
        Ok(body)
    }
}

/// Discover skills under every search directory. Unreadable directories are
/// skipped silently: a broken `~/.kiln/skills` should not stop the agent.
pub fn discover(repo_root: &Path) -> SkillSet {
    let mut set = SkillSet::default();
    for dir in search_dirs(repo_root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let source = if dir.starts_with(repo_root) {
            SkillSource::Project
        } else {
            SkillSource::Global
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let fallback = entry.file_name().to_string_lossy().to_string();
            if let Some(skill) = load_skill(&path, &fallback, source) {
                set.skills.entry(skill.name.clone()).or_insert(skill);
            }
        }
    }
    set
}

fn load_skill(dir: &Path, fallback_name: &str, source: SkillSource) -> Option<Skill> {
    let file = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&file).ok()?;
    let (meta, _) = frontmatter::split(&text);

    let name = meta
        .get("name")
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback_name.to_string());

    // Without a description the model cannot decide when to use it, which
    // defeats progressive disclosure. Skip and let `kiln skills` report it.
    let description = meta
        .get("description")
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())?;

    let allowed_tools = meta
        .get("allowed-tools")
        .or_else(|| meta.get("allowed_tools"))
        .map(|v| frontmatter::split_list(v))
        .unwrap_or_default();

    Some(Skill {
        name,
        description,
        path: file,
        source,
        allowed_tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, description: Option<&str>, body: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let header = match description {
            Some(d) => format!("---\nname: {name}\ndescription: {d}\n---\n"),
            None => String::new(),
        };
        std::fs::write(skill_dir.join("SKILL.md"), format!("{header}{body}")).unwrap();
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-skills-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovers_skills_and_builds_a_compact_catalogue() {
        let root = temp("catalogue");
        let dir = root.join(".kiln").join("skills");
        write_skill(
            &dir,
            "pdf",
            Some("Fill PDF forms"),
            "# Steps\nDo the thing.",
        );

        let set = discover(&root);
        assert_eq!(set.len(), 1);
        let catalogue = set.catalogue();
        assert!(catalogue.contains("pdf: Fill PDF forms"));
        // Progressive disclosure: the body must not leak into the catalogue.
        assert!(!catalogue.contains("Do the thing"));

        let body = set.load_body("pdf").unwrap();
        assert!(body.contains("Do the thing"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skills_without_a_description_are_skipped() {
        let root = temp("nodesc");
        let dir = root.join(".kiln").join("skills");
        write_skill(&dir, "broken", None, "body");
        write_skill(&dir, "good", Some("has one"), "body");
        let set = discover(&root);
        assert_eq!(set.len(), 1);
        assert!(set.get("good").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_skills_shadow_global_ones() {
        // Same registry shape the discoverer uses, exercised without touching
        // the real home directory.
        let mut set = SkillSet::default();
        let global = Skill {
            name: "deploy".into(),
            description: "global".into(),
            path: PathBuf::from("g"),
            source: SkillSource::Global,
            allowed_tools: Vec::new(),
        };
        let project = Skill {
            name: "deploy".into(),
            description: "project".into(),
            path: PathBuf::from("p"),
            source: SkillSource::Project,
            allowed_tools: Vec::new(),
        };
        set.skills.entry(global.name.clone()).or_insert(global);
        set.skills.entry(project.name.clone()).or_insert(project);
        assert_eq!(set.len(), 1);
        assert_eq!(set.get("deploy").unwrap().description, "global");
    }

    #[test]
    fn unknown_skill_reports_a_readable_error() {
        let set = SkillSet::default();
        let err = set.load_body("nope").unwrap_err();
        assert!(err.contains("unknown skill 'nope'"));
    }
}
