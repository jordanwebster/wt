use std::collections::{BTreeMap, BTreeSet};

use crate::{CoreError, ExitClass};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Part {
    Literal(String),
    Variable(String),
}

fn parse_parts(input: &str) -> Result<Vec<Part>, CoreError> {
    let chars: Vec<char> = input.chars().collect();
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' {
            literal.push(chars[index]);
            index += 1;
            continue;
        }
        if index + 1 >= chars.len() {
            literal.push('$');
            break;
        }
        if chars[index + 1] == '$' {
            literal.push('$');
            index += 2;
            continue;
        }
        let (name, next) = if chars[index + 1] == '{' {
            let Some(close) = chars[index + 2..].iter().position(|ch| *ch == '}') else {
                return Err(invalid("unclosed `${...}` variable"));
            };
            let close = index + 2 + close;
            (
                chars[index + 2..close].iter().collect::<String>(),
                close + 1,
            )
        } else if chars[index + 1].is_ascii_alphabetic() || chars[index + 1] == '_' {
            let mut end = index + 2;
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            (chars[index + 1..end].iter().collect::<String>(), end)
        } else {
            literal.push('$');
            index += 1;
            continue;
        };
        if name.is_empty()
            || !name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Err(invalid("invalid template variable name"));
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(&mut literal)));
        }
        parts.push(Part::Variable(name));
        index = next;
    }
    if !literal.is_empty() {
        parts.push(Part::Literal(literal));
    }
    Ok(parts)
}

fn invalid(message: &str) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        message,
        "fix the template syntax",
    )
}

pub fn validate(input: &str) -> Result<(), CoreError> {
    parse_parts(input).map(|_| ())
}

pub fn references(input: &str) -> Result<BTreeSet<String>, CoreError> {
    Ok(parse_parts(input)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Variable(name) => Some(name),
            Part::Literal(_) => None,
        })
        .collect())
}

/// Extracts template-shaped references from shell-owned text without rejecting
/// shell parameter expansions. Shell commands are never templates, but the
/// configuration validator still needs to recognise valid `$WT_*` occurrences.
pub fn shell_references(input: &str) -> BTreeSet<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut references = BTreeSet::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' || index + 1 >= chars.len() {
            index += 1;
            continue;
        }
        if chars[index + 1] == '{' {
            let Some(relative_close) = chars[index + 2..].iter().position(|ch| *ch == '}') else {
                index += 1;
                continue;
            };
            let close = index + 2 + relative_close;
            let name: String = chars[index + 2..close].iter().collect();
            if valid_variable_name(&name) {
                references.insert(name);
            }
            index = close + 1;
            continue;
        }
        if chars[index + 1] == '$' {
            index += 2;
            continue;
        }
        if chars[index + 1].is_ascii_alphabetic() || chars[index + 1] == '_' {
            let mut end = index + 2;
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            references.insert(chars[index + 1..end].iter().collect());
            index = end;
            continue;
        }
        index += 1;
    }
    references
}

fn valid_variable_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub fn expand(input: &str, context: &BTreeMap<String, String>) -> Result<String, CoreError> {
    let mut output = String::new();
    for part in parse_parts(input)? {
        match part {
            Part::Literal(value) => output.push_str(&value),
            Part::Variable(name) => match context.get(&name) {
                Some(value) => output.push_str(value),
                None => {
                    return Err(CoreError::new(
                        ExitClass::State,
                        "ENV_UNDEFINED",
                        format!("template variable `{name}` is undefined"),
                        format!("set `{name}` in the parent environment or remove the reference"),
                    ))
                }
            },
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_names_braces_dollars_and_literal_dollar() {
        let context = BTreeMap::from([("A".to_owned(), "one".to_owned())]);
        assert_eq!(expand("$A/${A}/$$/$-", &context).unwrap(), "one/one/$/$-");
    }

    #[test]
    fn rejects_unclosed_or_invalid_braces() {
        assert!(validate("${A").is_err());
        assert!(validate("${1}").is_err());
    }

    #[test]
    fn shell_scanner_keeps_valid_references_and_ignores_shell_expansions() {
        assert_eq!(
            shell_references("${h%??} $WT_ROOT ${WT_HOME} $$WT_BAD ${1} ${UNFINISHED"),
            BTreeSet::from(["WT_HOME".to_owned(), "WT_ROOT".to_owned()])
        );
    }
}
