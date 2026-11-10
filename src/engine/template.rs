use minijinja::{Environment, UndefinedBehavior, Value};
use serde::Serialize;

use crate::error::Error;

use super::budget::{Budget, Buffer};

pub(crate) fn environment() -> Environment<'static> {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment.set_keep_trailing_newline(true);
    environment
}

pub fn render(source: &str, context: impl Serialize) -> Result<String, minijinja::Error> {
    environment().render_str(source, context)
}

pub(super) fn render_bounded(
    source: &str,
    context: &Value,
    budget: &Budget,
    location: &str,
) -> Result<Buffer, Error> {
    let environment = environment();
    let mut buffer = budget.buffer();
    let result = environment.template_from_str(source).and_then(|template| {
        template
            .render_captured_to(context, &mut buffer)
            .map(|_| ())
    });
    result.map_err(|source| {
        buffer
            .limit_error(location)
            .unwrap_or_else(|| Error::Template {
                location: location.to_owned(),
                source,
            })
    })?;
    Ok(buffer)
}
