//! The feature manifest: every component the reference ships, plugged after the core.

use cordis::Runtime;

use crate::verbs;

pub fn plug_features(runtime: &mut Runtime) -> Result<(), cordis::Error> {
    runtime.plug(verbs::home::HomeVerbs)?;
    Ok(())
}
