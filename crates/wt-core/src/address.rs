use std::collections::BTreeSet;

use crate::{
    error::CoreError,
    model::{Label, Target},
    ExitClass,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveTree {
    pub target: Target,
    pub root: String,
}

pub fn containing(cwd: &str, trees: &[LiveTree]) -> Option<Target> {
    trees
        .iter()
        .filter(|tree| {
            cwd == tree.root
                || cwd
                    .strip_prefix(&tree.root)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|tree| tree.root.len())
        .map(|tree| tree.target.clone())
}

pub fn resolve(
    address: &str,
    cwd: &str,
    trees: &[LiveTree],
    labels: &BTreeSet<Label>,
) -> Result<Target, CoreError> {
    if address == "." {
        return containing(cwd, trees).ok_or_else(|| {
            not_found(
                "the current directory is not inside a live tree",
                Vec::new(),
            )
        });
    }
    if address.starts_with('/') {
        return containing(address, trees)
            .ok_or_else(|| not_found("no live tree has that path", Vec::new()));
    }
    if address.contains('/') {
        let target = Target::parse(address)?;
        return trees
            .iter()
            .any(|tree| tree.target == target)
            .then_some(target)
            .ok_or_else(|| not_found("target is not live", Vec::new()));
    }
    if let Some(current) = containing(cwd, trees) {
        let candidate = Target {
            label: current.label,
            name: address.to_owned(),
        };
        if trees.iter().any(|tree| tree.target == candidate) {
            return Ok(candidate);
        }
    }
    let label = Label::new(address)?;
    if labels.contains(&label) {
        return Ok(Target::canonical(label));
    }
    let candidates: Vec<String> = trees
        .iter()
        .filter(|tree| tree.target.name == address)
        .map(|tree| tree.target.to_string())
        .collect();
    Err(not_found("address was not found", candidates))
}

fn not_found(message: &str, candidates: Vec<String>) -> CoreError {
    let remedy = if candidates.is_empty() {
        "run `wt list` to list live targets".to_owned()
    } else {
        format!("use a fully qualified target: {}", candidates.join(", "))
    };
    CoreError::new(ExitClass::NotFound, "NOT_FOUND", message, remedy)
        .with_details(serde_json::json!({"candidates": candidates}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(label: &str, name: &str, root: &str) -> LiveTree {
        LiveTree {
            target: Target {
                label: Label::new(label).unwrap(),
                name: name.to_owned(),
            },
            root: root.to_owned(),
        }
    }

    #[test]
    fn bare_name_prefers_current_label_then_label() {
        let trees = vec![
            tree("a", "canonical", "/a"),
            tree("a", "work", "/trees/a/work"),
            tree("work", "canonical", "/work"),
        ];
        let labels = BTreeSet::from([Label::new("a").unwrap(), Label::new("work").unwrap()]);
        assert_eq!(
            resolve("work", "/a/src", &trees, &labels)
                .unwrap()
                .to_string(),
            "a/work"
        );
        assert_eq!(
            resolve("work", "/else", &trees, &labels)
                .unwrap()
                .to_string(),
            "work"
        );
    }

    #[test]
    fn cwd_uses_longest_prefix() {
        let trees = vec![tree("a", "canonical", "/a"), tree("a", "deep", "/a/deep")];
        assert_eq!(containing("/a/deep/src", &trees).unwrap().name, "deep");
    }

    #[test]
    fn absolute_paths_use_longest_prefix_and_not_found_lists_candidates() {
        let trees = vec![
            tree("a", "work", "/trees/a/work"),
            tree("b", "work", "/trees/b/work"),
        ];
        assert_eq!(
            resolve("/trees/a/work/src", "/else", &trees, &BTreeSet::new())
                .unwrap()
                .to_string(),
            "a/work"
        );
        let error = resolve("work", "/else", &trees, &BTreeSet::new()).unwrap_err();
        assert_eq!(error.code.0, "NOT_FOUND");
        assert_eq!(
            error.details["candidates"],
            serde_json::json!(["a/work", "b/work"])
        );
    }
}
