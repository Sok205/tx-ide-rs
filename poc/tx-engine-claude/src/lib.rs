//! The Claude Code engine for tx-core: `claude [--resume ID] [--model M] --effort E --no-chrome
//! --append-system-prompt <profile text> --dangerously-skip-permissions [prompt]`, with every
//! granted skill linked under `.claude/skills/<name>` in the working directory.

use std::path::PathBuf;

use tx_core::{Effort, EngineAdapter, EngineError, LaunchPlan, LaunchRequest, WorkspaceAction};

pub const CLAUDE_BIN: &str = "claude";
pub const SKILLS_DIR: &str = ".claude/skills";
pub const MODEL_FLAG: &str = "--model";
pub const RESUME_FLAG: &str = "--resume";
pub const EFFORT_FLAG: &str = "--effort";
pub const NO_CHROME_FLAG: &str = "--no-chrome";
pub const APPEND_SYSTEM_PROMPT_FLAG: &str = "--append-system-prompt";
pub const SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";
pub const DEFAULT_EFFORT: Effort = Effort::High;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Claude;

impl EngineAdapter for Claude {
    fn id(&self) -> &str {
        "claude"
    }

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let mut args = Vec::new();
        if let Some(resume) = req.resume.filter(|resume| !resume.is_empty()) {
            args.extend([RESUME_FLAG.to_owned(), resume.to_owned()]);
        }
        if let Some(model) = req.model.filter(|model| !model.is_empty()) {
            args.extend([MODEL_FLAG.to_owned(), model.to_owned()]);
        }
        args.extend([
            EFFORT_FLAG.to_owned(),
            req.effort.unwrap_or(DEFAULT_EFFORT).as_str().to_owned(),
            NO_CHROME_FLAG.to_owned(),
        ]);
        if !req.profile.text().is_empty() {
            args.extend([
                APPEND_SYSTEM_PROMPT_FLAG.to_owned(),
                req.profile.text().to_owned(),
            ]);
        }
        args.push(SKIP_PERMISSIONS_FLAG.to_owned());
        if let Some(prompt) = req.initial_prompt.filter(|prompt| !prompt.is_empty()) {
            args.push(prompt.to_owned());
        }
        let mut plan = LaunchPlan::new(CLAUDE_BIN).args(args);
        for link in skill_links(SKILLS_DIR, req.profile.skill_dirs()) {
            plan = plan.workspace(link);
        }
        Ok(plan)
    }
}

/// One symlink per granted skill dir: `<engine dir>/<skill name>` → the skill dir.
pub fn skill_links(engine_dir: &str, skill_dirs: &[PathBuf]) -> Vec<WorkspaceAction> {
    skill_dirs
        .iter()
        .filter_map(|dir| {
            let name = dir.file_name()?;
            Some(WorkspaceAction::Symlink {
                src: dir.clone(),
                dst: PathBuf::from(engine_dir).join(name),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tx_core::{Profile, ResolvedProfile};

    fn profile(text: &str, skills: &[&str]) -> (tempfile::TempDir, Profile) {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::write(
            ResolvedProfile {
                text: text.into(),
                skill_dirs: skills.iter().map(PathBuf::from).collect(),
            },
            &dir.path().join("p.md"),
        )
        .unwrap();
        (dir, profile)
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn id_is_claude() {
        assert_eq!(Claude.id(), "claude");
    }

    #[test]
    fn defaults() {
        let (dir, profile) = profile("", &[]);
        let plan = Claude
            .launch(&LaunchRequest::new(dir.path(), &profile))
            .unwrap();
        assert_eq!(plan.program, "claude");
        assert_eq!(
            plan.args,
            argv(&["--effort", "high", "--no-chrome", "--dangerously-skip-permissions"])
        );
        assert!(plan.env.is_empty());
        assert!(plan.workspace.is_empty());
    }

    #[test]
    fn every_option() {
        let (dir, profile) = profile("# Role\nbe good", &["/skills/one", "/other/two"]);
        let plan = Claude
            .launch(&LaunchRequest::new(dir.path(), &profile).model("opus").effort(Effort::Medium).initial_prompt("go"))
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--model",
                "opus",
                "--effort",
                "medium",
                "--no-chrome",
                "--append-system-prompt",
                "# Role\nbe good",
                "--dangerously-skip-permissions",
                "go",
            ])
        );
        assert_eq!(
            plan.workspace,
            vec![
                WorkspaceAction::Symlink {
                    src: "/skills/one".into(),
                    dst: ".claude/skills/one".into()
                },
                WorkspaceAction::Symlink {
                    src: "/other/two".into(),
                    dst: ".claude/skills/two".into()
                },
            ]
        );
    }

    #[test]
    fn empty_model_and_prompt_are_absent() {
        let (dir, profile) = profile("", &[]);
        let plan = Claude
            .launch(&LaunchRequest::new(dir.path(), &profile).model("").effort(Effort::Max).initial_prompt(""))
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&["--effort", "max", "--no-chrome", "--dangerously-skip-permissions"])
        );
    }

    #[test]
    fn resume_continues_a_conversation() {
        let (dir, profile) = profile("role", &[]);
        let plan = Claude
            .launch(&LaunchRequest::new(dir.path(), &profile).resume("chat-42").initial_prompt("more"))
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--resume",
                "chat-42",
                "--effort",
                "high",
                "--no-chrome",
                "--append-system-prompt",
                "role",
                "--dangerously-skip-permissions",
                "more",
            ])
        );
        let plan = Claude
            .launch(&LaunchRequest::new(dir.path(), &profile).resume(""))
            .unwrap();
        assert!(!plan.args.iter().any(|a| a == "--resume"), "empty resume is absent");
    }

    #[test]
    fn a_skill_dir_without_a_name_is_skipped() {
        assert!(skill_links(".x", &[PathBuf::from("/")]).is_empty());
        assert_eq!(skill_links(".x", &[PathBuf::from("/a/b/")]).len(), 1);
        let _ = Path::new("");
    }
}
