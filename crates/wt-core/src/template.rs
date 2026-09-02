use std::collections::{BTreeMap, BTreeSet};

use crate::{CoreError, ExitClass};

pub const FUNCTIONS: &[&str] = &[
    "home",
    "root",
    "repo",
    "branch",
    "label",
    "name",
    "name_snake",
    "name_short",
    "target",
];

/// The functions a `branch` template may call. A branch is chosen before the
/// worktree exists, so nothing that describes a materialised tree — its root,
/// its ports, its `vars` — has a value yet, and `branch()` is the value being
/// computed.
pub const BRANCH_FUNCTIONS: &[&str] = &["label", "name", "name_snake", "name_short"];

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Call {
    Simple(String),
}

impl Call {
    pub fn name(&self) -> &str {
        match self {
            Self::Simple(name) => name,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Simple(name) => format!("{name}()"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Expression {
    Var(String),
    Port(String),
    Meta(String),
    Call(Call),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Part {
    Literal(String),
    Expression(Expression),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionValues {
    pub simple: BTreeMap<String, String>,
    pub ports: BTreeMap<String, String>,
    /// Creation metadata, populated only where `meta.<key>` is legal.
    pub meta: BTreeMap<String, String>,
}

pub struct Context<'a> {
    pub vars: &'a BTreeMap<String, String>,
    pub functions: &'a FunctionValues,
}

fn parse_parts(input: &str) -> Result<Vec<Part>, CoreError> {
    let chars: Vec<char> = input.chars().collect();
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '{' || chars.get(index + 1) != Some(&'{') {
            literal.push(chars[index]);
            index += 1;
            continue;
        }
        let Some(relative_close) = chars[index + 2..]
            .windows(2)
            .position(|pair| pair == ['}', '}'])
        else {
            return Err(invalid("unclosed `{{...}}` expression"));
        };
        let close = index + 2 + relative_close;
        let body = chars[index + 2..close].iter().collect::<String>();
        let expression = parse_expression(&body)?;
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(&mut literal)));
        }
        parts.push(Part::Expression(expression));
        index = close + 2;
    }
    if !literal.is_empty() {
        parts.push(Part::Literal(literal));
    }
    Ok(parts)
}

fn parse_expression(input: &str) -> Result<Expression, CoreError> {
    if input.chars().any(char::is_whitespace) {
        return Err(invalid("template expressions may not contain whitespace"));
    }
    if let Some(name) = input.strip_prefix("ports.") {
        if valid_port_name(name) {
            return Ok(Expression::Port(name.to_owned()));
        }
        return Err(invalid("invalid port reference in `{{...}}`"));
    }
    if let Some(key) = input.strip_prefix("meta.") {
        if valid_meta_key(key) {
            return Ok(Expression::Meta(key.to_owned()));
        }
        return Err(invalid("invalid metadata reference in `{{...}}`"));
    }
    if valid_identifier(input) {
        return Ok(Expression::Var(input.to_owned()));
    }
    let Some(open) = input.find('(') else {
        return Err(invalid("invalid `{{...}}` expression"));
    };
    let Some(arguments) = input.strip_suffix(')') else {
        return Err(invalid("invalid function call in `{{...}}`"));
    };
    let name = &input[..open];
    if !valid_identifier(name) {
        return Err(invalid("invalid function name in `{{...}}`"));
    }
    let arguments = &arguments[open + 1..];
    if !arguments.is_empty() {
        return Err(invalid("template function takes no arguments"));
    }
    Ok(Expression::Call(Call::Simple(name.to_owned())))
}

fn invalid(message: &str) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        message,
        "fix the template syntax",
    )
}

fn vars_unknown(message: impl Into<String>) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "VARS_UNKNOWN",
        message,
        "declare the variable in `vars` or correct the reference",
    )
}

pub fn validate(input: &str) -> Result<(), CoreError> {
    if let Some(key) = meta_references(input)?.into_iter().next() {
        return Err(meta_out_of_place(&key));
    }
    Ok(())
}

pub fn meta_references(input: &str) -> Result<BTreeSet<String>, CoreError> {
    Ok(parse_parts(input)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Expression(Expression::Meta(key)) => Some(key),
            Part::Literal(_)
            | Part::Expression(Expression::Var(_))
            | Part::Expression(Expression::Port(_))
            | Part::Expression(Expression::Call(_)) => None,
        })
        .collect())
}

/// Checks a `branch` template against the references a branch may make.
pub fn validate_branch(input: &str) -> Result<(), CoreError> {
    for part in parse_parts(input)? {
        match part {
            Part::Literal(_) | Part::Expression(Expression::Meta(_)) => {}
            Part::Expression(Expression::Var(name)) => {
                return Err(branch_out_of_reach(&format!("vars name `{name}`")))
            }
            Part::Expression(Expression::Port(name)) => {
                return Err(branch_out_of_reach(&format!("port `ports.{name}`")))
            }
            Part::Expression(Expression::Call(call)) => {
                if !BRANCH_FUNCTIONS.contains(&call.name()) {
                    return Err(if FUNCTIONS.contains(&call.name()) {
                        branch_out_of_reach(&format!("function `{}`", call.display()))
                    } else {
                        invalid(&format!("unknown template function `{}`", call.display()))
                    });
                }
            }
        }
    }
    Ok(())
}

fn meta_out_of_place(key: &str) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        format!("`meta.{key}` is available only in a `branch` template"),
        "reference metadata from `branch`, or declare the value in `vars`",
    )
}

fn branch_out_of_reach(reference: &str) -> CoreError {
    CoreError::new(
        ExitClass::State,
        "CONFIG_INVALID",
        format!("a `branch` template cannot reference the {reference}: the branch is chosen before the worktree exists"),
        "use `meta.<key>`, `name()`, `name_snake()`, `name_short()`, or `label()`",
    )
}

pub fn references(input: &str) -> Result<BTreeSet<String>, CoreError> {
    Ok(parse_parts(input)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Expression(Expression::Var(name)) => Some(name),
            Part::Literal(_)
            | Part::Expression(Expression::Port(_))
            | Part::Expression(Expression::Meta(_))
            | Part::Expression(Expression::Call(_)) => None,
        })
        .collect())
}

pub fn calls(input: &str) -> Result<BTreeSet<Call>, CoreError> {
    Ok(parse_parts(input)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Expression(Expression::Call(call)) => Some(call),
            Part::Literal(_)
            | Part::Expression(Expression::Var(_))
            | Part::Expression(Expression::Meta(_))
            | Part::Expression(Expression::Port(_)) => None,
        })
        .collect())
}

pub fn port_references(input: &str) -> Result<BTreeSet<String>, CoreError> {
    Ok(parse_parts(input)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Expression(Expression::Port(name)) => Some(name),
            Part::Literal(_)
            | Part::Expression(Expression::Var(_))
            | Part::Expression(Expression::Meta(_))
            | Part::Expression(Expression::Call(_)) => None,
        })
        .collect())
}

pub fn validate_calls(input: &str, declared_ports: &BTreeSet<String>) -> Result<(), CoreError> {
    for call in calls(input)? {
        match &call {
            Call::Simple(name) if FUNCTIONS.contains(&name.as_str()) => {}
            Call::Simple(_) => {
                return Err(invalid(&format!(
                    "unknown template function `{}`",
                    call.display()
                )))
            }
        }
    }
    if let Some(name) = port_references(input)?
        .into_iter()
        .find(|name| !declared_ports.contains(name))
    {
        return Err(invalid(&format!(
            "port reference `ports.{name}` names an undeclared port"
        )));
    }
    Ok(())
}

pub fn expand(input: &str, context: &Context<'_>) -> Result<String, CoreError> {
    let mut output = String::new();
    for part in parse_parts(input)? {
        match part {
            Part::Literal(value) => output.push_str(&value),
            Part::Expression(Expression::Var(name)) => match context.vars.get(&name) {
                Some(value) => output.push_str(value),
                None => return Err(vars_unknown(format!("unknown vars name `{name}`"))),
            },
            Part::Expression(Expression::Port(name)) => {
                output.push_str(context.functions.ports.get(&name).ok_or_else(|| {
                    invalid(&format!(
                        "port reference `ports.{name}` names an undeclared port"
                    ))
                })?)
            }
            Part::Expression(Expression::Meta(key)) => match context.functions.meta.get(&key) {
                Some(value) => output.push_str(value),
                None => return Err(meta_out_of_place(&key)),
            },
            Part::Expression(Expression::Call(call)) => {
                output.push_str(&function_value(&call, context.functions)?)
            }
        }
    }
    Ok(output)
}

fn function_value(call: &Call, values: &FunctionValues) -> Result<String, CoreError> {
    match call {
        Call::Simple(name) if FUNCTIONS.contains(&name.as_str()) => values
            .simple
            .get(name)
            .cloned()
            .ok_or_else(|| invalid(&format!("function `{}` has no value", call.display()))),
        Call::Simple(_) => Err(invalid(&format!(
            "unknown template function `{}`",
            call.display()
        ))),
    }
}

pub fn resolve_vars(
    declarations: &BTreeMap<String, String>,
    functions: &FunctionValues,
) -> Result<BTreeMap<String, String>, CoreError> {
    fn visit(
        key: &str,
        declarations: &BTreeMap<String, String>,
        functions: &FunctionValues,
        resolved: &mut BTreeMap<String, String>,
        active: &mut Vec<String>,
    ) -> Result<String, CoreError> {
        if let Some(value) = resolved.get(key) {
            return Ok(value.clone());
        }
        if let Some(start) = active.iter().position(|candidate| candidate == key) {
            let mut cycle = active[start..].to_vec();
            cycle.push(key.to_owned());
            cycle.sort();
            cycle.dedup();
            return Err(CoreError::new(
                ExitClass::State,
                "VARS_CYCLE",
                format!("vars cycle involves {}", cycle.join(", ")),
                "break the cycle in `vars`",
            ));
        }
        let value = declarations
            .get(key)
            .ok_or_else(|| vars_unknown(format!("unknown vars name `{key}`")))?;
        active.push(key.to_owned());
        let mut dependencies = BTreeMap::new();
        for dependency in references(value)? {
            if !declarations.contains_key(&dependency) {
                return Err(vars_unknown(format!(
                    "vars `{key}` references unknown name `{dependency}`"
                )));
            }
            dependencies.insert(
                dependency.clone(),
                visit(&dependency, declarations, functions, resolved, active)?,
            );
        }
        let expanded = expand(
            value,
            &Context {
                vars: &dependencies,
                functions,
            },
        )?;
        active.pop();
        resolved.insert(key.to_owned(), expanded.clone());
        Ok(expanded)
    }

    let mut resolved = BTreeMap::new();
    for key in declarations.keys() {
        visit(key, declarations, functions, &mut resolved, &mut Vec::new())?;
    }
    Ok(resolved)
}

/// Extracts shell variables used by resource recipes. Parameter operators are
/// deliberately ignored; this scan only protects scope-stripped snapshots.
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
            if valid_identifier(&name) {
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

fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn valid_meta_key(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase() || ch == '_')
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn valid_port_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn functions() -> FunctionValues {
        FunctionValues {
            simple: BTreeMap::from([
                ("home".to_owned(), "/home".to_owned()),
                ("root".to_owned(), "/tree".to_owned()),
                ("name".to_owned(), "feature".to_owned()),
            ]),
            ports: BTreeMap::from([("http".to_owned(), "20016".to_owned())]),
            meta: BTreeMap::new(),
        }
    }

    #[test]
    fn sole_evaluation_form_distinguishes_vars_functions_and_literal_dollars() {
        let vars = BTreeMap::from([("prefix".to_owned(), "one".to_owned())]);
        let functions = functions();
        let context = Context {
            vars: &vars,
            functions: &functions,
        };
        assert_eq!(
            expand("{{prefix}}/{{root()}}/{{ports.http}}/$$/$-", &context).unwrap(),
            "one//tree/20016/$$/$-"
        );
    }

    #[test]
    fn metadata_reads_only_inside_a_branch_template() {
        let error = validate("{{meta.ticket}}_{{name()}}").unwrap_err();
        assert_eq!(error.code.0, "CONFIG_INVALID");
        assert!(error.message.contains("`meta.ticket`"), "{}", error.message);
        validate_branch("{{meta.ticket}}_{{name()}}").unwrap();
        assert_eq!(
            meta_references("{{meta.ticket}}/{{meta.owner}}/{{name()}}").unwrap(),
            BTreeSet::from(["owner".to_owned(), "ticket".to_owned()])
        );

        let vars = BTreeMap::new();
        let functions = FunctionValues {
            simple: BTreeMap::from([("name".to_owned(), "fix-scroll".to_owned())]),
            ports: BTreeMap::new(),
            meta: BTreeMap::from([("ticket".to_owned(), "ABC-42".to_owned())]),
        };
        assert_eq!(
            expand(
                "{{meta.ticket}}_{{name()}}",
                &Context {
                    vars: &vars,
                    functions: &functions,
                },
            )
            .unwrap(),
            "ABC-42_fix-scroll"
        );
    }

    #[test]
    fn a_branch_template_reaches_only_what_exists_before_the_tree_does() {
        for (input, expected) in [
            ("{{prefix}}-{{name()}}", "vars name `prefix`"),
            ("{{ports.http}}", "port `ports.http`"),
            ("{{root()}}/x", "function `root()`"),
            ("{{branch()}}", "function `branch()`"),
        ] {
            let error = validate_branch(input).unwrap_err();
            assert_eq!(error.code.0, "CONFIG_INVALID");
            assert!(error.message.contains(expected), "{}", error.message);
        }
        let error = validate_branch("{{ticket()}}").unwrap_err();
        assert!(
            error.message.contains("unknown template function"),
            "{}",
            error.message
        );
        for allowed in [
            "{{label()}}",
            "{{name()}}",
            "{{name_snake()}}",
            "{{name_short()}}",
        ] {
            validate_branch(allowed).unwrap();
        }
    }

    #[test]
    fn every_dollar_spelling_is_literal() {
        let input = "$WT_PORT_HTTP/${HOME}/$WT_REPO/$HOME/$DATABASE_URL/$_private/$$";
        assert_eq!(
            expand(
                input,
                &Context {
                    vars: &BTreeMap::new(),
                    functions: &functions()
                }
            )
            .unwrap(),
            input
        );
    }

    #[test]
    fn double_open_brace_is_always_strict_template_syntax() {
        for invalid_input in [
            "{{",
            "{{name}",
            "{{ name}}",
            "{{name }}",
            "{{root( )}}",
            "{{ports.HTTP}}",
            "{{root(arg)}}",
        ] {
            let error = validate(invalid_input).unwrap_err();
            assert_eq!(error.code.0, "CONFIG_INVALID", "{invalid_input}");
        }
    }

    #[test]
    fn dag_resolution_is_independent_of_declaration_order() {
        let declarations = BTreeMap::from([
            ("last".to_owned(), "{{middle}}/c".to_owned()),
            ("first".to_owned(), "a".to_owned()),
            ("middle".to_owned(), "{{first}}/b".to_owned()),
        ]);
        let resolved = resolve_vars(&declarations, &functions()).unwrap();
        assert_eq!(resolved["last"], "a/b/c");
    }

    #[test]
    fn cycles_unknown_names_and_closed_functions_are_distinct() {
        let cycle = BTreeMap::from([
            ("a".to_owned(), "{{b}}".to_owned()),
            ("b".to_owned(), "{{a}}".to_owned()),
        ]);
        assert_eq!(
            resolve_vars(&cycle, &functions()).unwrap_err().code.0,
            "VARS_CYCLE"
        );
        let unknown = BTreeMap::from([("a".to_owned(), "{{missing}}".to_owned())]);
        assert_eq!(
            resolve_vars(&unknown, &functions()).unwrap_err().code.0,
            "VARS_UNKNOWN"
        );
        assert!(validate_calls("{{mystery()}}", &BTreeSet::new()).is_err());
        assert!(validate_calls("{{ports.missing}}", &BTreeSet::new()).is_err());
    }

    #[test]
    fn dotted_ports_are_lookups_and_ports_is_not_a_var() {
        let functions = functions();
        let context = Context {
            vars: &BTreeMap::new(),
            functions: &functions,
        };
        assert_eq!(
            expand("{{ports.http}}:{{ports.http}}", &context).unwrap(),
            "20016:20016"
        );
        assert!(validate_calls("{{ports.missing}}", &BTreeSet::new()).is_err());
    }

    #[test]
    fn old_template_spelling_is_literal_without_a_hint() {
        let input = "${root()}";
        assert_eq!(
            expand(
                input,
                &Context {
                    vars: &BTreeMap::new(),
                    functions: &functions(),
                },
            )
            .unwrap(),
            input
        );
    }
}
