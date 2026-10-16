//! The Codex engine for tx-core: `codex [resume ID] -m <model> -c model_reasoning_effort=<effort>
//! [-c developer_instructions=<profile text>] --dangerously-bypass-approvals-and-sandbox
//! --dangerously-bypass-hook-trust [prompt]`, with granted skills linked under `.agents/skills`.

use std::path::PathBuf;

use tx_core::{Effort, EngineAdapter, EngineError, LaunchPlan, LaunchRequest, WorkspaceAction};

pub const CODEX_BIN: &str = "codex";
pub const SKILLS_DIR: &str = ".agents/skills";
pub const CODEX_MODEL: &str = "gpt-5.6-sol";
pub const RESUME_SUBCOMMAND: &str = "resume";
pub const REASONING_EFFORT_KEY: &str = "model_reasoning_effort";
pub const DEVELOPER_INSTRUCTIONS_KEY: &str = "developer_instructions";
pub const BYPASS_APPROVALS_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";
pub const BYPASS_HOOK_TRUST_FLAG: &str = "--dangerously-bypass-hook-trust";
pub const DEFAULT_EFFORT: Effort = Effort::High;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Codex;

impl EngineAdapter for Codex {
    fn id(&self) -> &str {
        "codex"
    }

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let model = req
            .model
            .filter(|model| !model.is_empty())
            .unwrap_or(CODEX_MODEL);
        let effort = req.effort.unwrap_or(DEFAULT_EFFORT);
        let mut args = Vec::new();
        if let Some(resume) = req.resume.filter(|resume| !resume.is_empty()) {
            args.extend([RESUME_SUBCOMMAND.to_owned(), resume.to_owned()]);
        }
        args.extend([
            "-m".to_owned(),
            model.to_owned(),
            "-c".to_owned(),
            format!("{REASONING_EFFORT_KEY}={}", effort.as_str()),
        ]);
        if !req.profile.text().is_empty() {
            args.extend([
                "-c".to_owned(),
                format!("{DEVELOPER_INSTRUCTIONS_KEY}={}", req.profile.text()),
            ]);
        }
        args.extend([
            BYPASS_APPROVALS_FLAG.to_owned(),
            BYPASS_HOOK_TRUST_FLAG.to_owned(),
        ]);
        if let Some(prompt) = req.initial_prompt.filter(|prompt| !prompt.is_empty()) {
            args.push(prompt.to_owned());
        }
        let mut plan = LaunchPlan::new(CODEX_BIN).args(args);
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
    fn id_is_codex() {
        assert_eq!(Codex.id(), "codex");
    }

    #[test]
    fn defaults() {
        let (dir, profile) = profile("", &[]);
        let plan = Codex
            .launch(&LaunchRequest::new(dir.path(), &profile))
            .unwrap();
        assert_eq!(plan.program, "codex");
        assert_eq!(
            plan.args,
            argv(&[
                "-m",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=high",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ])
        );
        assert!(plan.workspace.is_empty());
    }

    #[test]
    fn every_option() {
        let (dir, profile) = profile("be good", &["/skills/one"]);
        let plan = Codex
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .model("o3")
                    .effort(Effort::Low)
                    .initial_prompt("go"),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "-m",
                "o3",
                "-c",
                "model_reasoning_effort=low",
                "-c",
                "developer_instructions=be good",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
                "go",
            ])
        );
        assert_eq!(
            plan.workspace,
            vec![WorkspaceAction::Symlink {
                src: "/skills/one".into(),
                dst: ".agents/skills/one".into()
            }]
        );
    }

    #[test]
    fn resume_is_the_subcommand() {
        let (dir, profile) = profile("", &[]);
        let plan = Codex
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .resume("sess-7")
                    .model("o3"),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "resume",
                "sess-7",
                "-m",
                "o3",
                "-c",
                "model_reasoning_effort=high",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ])
        );
        let plan = Codex
            .launch(&LaunchRequest::new(dir.path(), &profile).resume(""))
            .unwrap();
        assert_eq!(plan.args[0], "-m", "empty resume is absent");
    }

    #[test]
    fn empty_model_and_prompt_fall_back() {
        let (dir, profile) = profile("", &[]);
        let plan = Codex
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .model("")
                    .initial_prompt(""),
            )
            .unwrap();
        assert_eq!(plan.args[1], "gpt-5.6-sol");
        assert_eq!(plan.args.len(), 6);
    }
}
