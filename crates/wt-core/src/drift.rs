use std::collections::BTreeSet;

/// Returns the changed default-branch paths that match at least one declared
/// sync input. Git supplies names; this function only applies the pure pathspec
/// decision and stable ordering required by SPEC §12.
pub fn drift(diff_names: &[String], sync_inputs: &[String]) -> Vec<String> {
    diff_names
        .iter()
        .filter(|path| sync_inputs.iter().any(|pattern| matches(pattern, path)))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn matches(pattern: &str, path: &str) -> bool {
    let text: Vec<char> = path.chars().collect();
    let mut states = BTreeSet::from([0usize]);
    for token in pattern.chars() {
        if token == '*' {
            let mut expanded = states.clone();
            for start in states {
                expanded.extend(start..=text.len());
            }
            states = expanded;
        } else {
            states = states
                .into_iter()
                .filter_map(|index| (text.get(index) == Some(&token)).then_some(index + 1))
                .collect();
        }
        if states.is_empty() {
            return false;
        }
    }
    states.contains(&text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_changed_sync_inputs_in_lexical_order() {
        let changed = vec![
            "src/lib.rs".to_owned(),
            "z.csproj".to_owned(),
            "Cargo.toml".to_owned(),
            "a.csproj".to_owned(),
        ];
        let inputs = vec!["Cargo.toml".to_owned(), "*.csproj".to_owned()];
        assert_eq!(
            drift(&changed, &inputs),
            ["Cargo.toml", "a.csproj", "z.csproj"]
        );
    }
}
