//! `{{name}}` substitution for env values, rendered files, pool names and
//! branch templates. There is deliberately nothing else: no functions, no
//! conditionals, no escapes. A file that needs literal `{{` is copied with
//! `copy` instead of rendered.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// Replaces every `{{ variable }}` in `text`. `what` names the thing being
/// rendered, for the error message.
pub fn render(text: &str, vars: &BTreeMap<String, String>, what: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(Error::Config(format!(
                "{what}: `{{{{` without a closing `}}}}`"
            )));
        };
        let key = after[..end].trim();
        match vars.get(key) {
            Some(value) => out.push_str(value),
            None => return Err(unknown(key, vars, what)),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn unknown(key: &str, vars: &BTreeMap<String, String>, what: &str) -> Error {
    let available = vars.keys().cloned().collect::<Vec<_>>().join(", ");
    Error::Config(format!(
        "{what}: unknown template variable `{key}`; available: {available}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("name".to_owned(), "foo".to_owned()),
            ("ports.http".to_owned(), "20001".to_owned()),
        ])
    }

    #[test]
    fn substitutes_and_trims() {
        let out = render("{{name}}:{{ ports.http }}", &vars(), "test").unwrap();
        assert_eq!(out, "foo:20001");
    }

    #[test]
    fn unknown_variable_lists_what_exists() {
        let error = render("{{nope}}", &vars(), "env.X")
            .unwrap_err()
            .to_string();
        assert!(error.contains("env.X"), "{error}");
        assert!(error.contains("ports.http"), "{error}");
    }

    #[test]
    fn unterminated_is_an_error() {
        assert!(render("a {{name", &vars(), "test").is_err());
    }
}
