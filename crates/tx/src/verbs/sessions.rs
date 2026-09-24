//! Verb component (pending port).

use cordis::{BoxError, Component, Ctx};

pub struct SessionVerbs;

impl Component for SessionVerbs {
    fn name(&self) -> &str {
        "verbs.sessions"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands"]
    }
    fn apply(&self, _ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        Ok(())
    }
}
