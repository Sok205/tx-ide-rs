//! Verb component (pending port).

use cordis::{BoxError, Component, Ctx};

pub struct ArtifactVerbs;

impl Component for ArtifactVerbs {
    fn name(&self) -> &str {
        "verbs.artifacts"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands"]
    }
    fn apply(&self, _ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        Ok(())
    }
}
