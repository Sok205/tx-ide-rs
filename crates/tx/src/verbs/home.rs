//! `_init-home`: the installer's seam over `ensure_home`.

use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, HOME, Visibility};
use crate::storage::Home;

pub struct HomeVerbs;

impl Component for HomeVerbs {
    fn name(&self) -> &str {
        "verbs.home"
    }
    fn inject(&self) -> &[&'static str] {
        &["home", "commands"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let home = ctx.get(HOME)?;
        ctx.get(COMMANDS)?.register(ctx, Visibility::Hidden, InitHome(home));
        Ok(())
    }
}

struct InitHome(Rc<Home>);

impl Command for InitHome {
    fn name(&self) -> &'static str {
        "_init-home"
    }
    fn summary(&self) -> &'static str {
        "Internal: create the $TX_IDE_HOME skeleton (idempotent) — the installer's seam."
    }
    fn run(&self, _argv: &[String]) -> Result<i32, BoxError> {
        self.0.ensure()?;
        println!("initialized $TX_IDE_HOME skeleton at {}", self.0.root().display());
        Ok(0)
    }
}
