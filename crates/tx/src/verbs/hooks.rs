//! Verb component (pending port).

use cordis::{BoxError, Component, Ctx};

pub struct HookVerbs;

impl Component for HookVerbs {
    fn name(&self) -> &str {
        "verbs.hooks"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands"]
    }
    fn apply(&self, _ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        Ok(())
    }
}
