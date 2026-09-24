//! Verb component (pending port).

use cordis::{BoxError, Component, Ctx};

pub struct ListingVerbs;

impl Component for ListingVerbs {
    fn name(&self) -> &str {
        "verbs.listing"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands"]
    }
    fn apply(&self, _ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        Ok(())
    }
}
