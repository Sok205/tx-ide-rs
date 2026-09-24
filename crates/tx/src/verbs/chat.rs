//! Verb component (pending port).

use cordis::{BoxError, Component, Ctx};

pub struct ChatVerbs;

impl Component for ChatVerbs {
    fn name(&self) -> &str {
        "verbs.chat"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands"]
    }
    fn apply(&self, _ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        Ok(())
    }
}
