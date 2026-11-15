//! The `use_skill` tool: how the model actually pulls a skill body in.
//!
//! The system prompt lists skill names and descriptions; this tool is the only
//! path to the content. Loading is a read-only operation and runs without an
//! approval prompt.

use crate::SkillSet;
use async_trait::async_trait;
use kiln_core::Risk;
use kiln_tools::{Tool, ToolCtx, ToolOutcome, ToolSpec};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct UseSkill {
    skills: Arc<SkillSet>,
}

impl UseSkill {
    pub fn new(skills: Arc<SkillSet>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for UseSkill {
    fn spec(&self) -> ToolSpec {
        let available = if self.skills.is_empty() {
            "none installed".to_string()
        } else {
            self.skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        ToolSpec {
            name: "use_skill".to_string(),
            description: format!(
                "Load the full instructions for a skill. Use this when the task matches a \
                 skill description in your system prompt. Available: {available}."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill name, exactly as listed in the system prompt"
                    }
                },
                "required": ["name"]
            }),
            risk: Risk::Read,
        }
    }

    async fn run(&self, args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolOutcome::err("missing required argument 'name'");
        };
        match self.skills.load_body(name) {
            Ok(body) => ToolOutcome::ok(format!("# Skill: {name}\n\n{body}")),
            Err(e) => ToolOutcome::err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover;
    use kiln_core::config::ToolsConfig;

    #[tokio::test]
    async fn loads_a_skill_body_on_demand() {
        let root = std::env::temp_dir().join(format!("kiln-useskill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(".kiln").join("skills").join("greet");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: greet\ndescription: Say hello properly\n---\nSay hello, then bow.",
        )
        .unwrap();

        let skills = Arc::new(discover(&root));
        let tool = UseSkill::new(skills);
        assert!(tool.spec().description.contains("greet"));

        let ctx = ToolCtx::new(&root, &ToolsConfig::default()).unwrap();
        let out = tool.run(&json!({"name": "greet"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Say hello, then bow."));

        let missing = tool.run(&json!({"name": "nope"}), &ctx).await;
        assert!(missing.is_error);
        assert!(missing.content.contains("unknown skill"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
