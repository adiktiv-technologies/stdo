use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;

pub(crate) fn environment() -> Environment<'static> {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment
}

pub fn render(source: &str, context: impl Serialize) -> Result<String, minijinja::Error> {
    environment().render_str(source, context)
}
