use std::collections::BTreeSet;

use crate::{config::TiedTo, model::EnvMap, resource::ExpandedCommand};

const TREE_SPECIFIC_KEYS: &[&str] = &[
    "WT_ROOT",
    "WT_TARGET",
    "WT_NAME",
    "WT_NAME_SNAKE",
    "WT_NAME_SHORT",
    "WT_SLOT",
    "WT_SESSION",
    "WT_BIN",
    "WT_PATH_PREFIX",
    "PATH",
];

/// Selects exactly the environment keys persisted in a resource snapshot.
pub fn minimise_env(
    assembled: &EnvMap,
    declared_aliases: &[String],
    task_env_keys: &[String],
    snapshot_env: &[String],
    tied_to: TiedTo,
) -> EnvMap {
    let explicit: BTreeSet<&str> = declared_aliases
        .iter()
        .chain(task_env_keys)
        .chain(snapshot_env)
        .map(String::as_str)
        .collect();
    assembled
        .iter()
        .filter(|(key, _)| {
            key.as_str() != "WT_ACTIVATION"
                && (key.starts_with("WT_")
                    || key.as_str() == "PATH"
                    || explicit.contains(key.as_str()))
                && (tied_to != TiedTo::Repo || !is_tree_specific_key(key))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub(crate) fn is_tree_specific_key(key: &str) -> bool {
    TREE_SPECIFIC_KEYS.contains(&key)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MissingTreeVerdict {
    Run,
    OrphanedExeMissing { executable: String },
}

/// Applies A25 to only the expanded recipe that is about to run.
pub fn missing_tree_verdict(recipe: &ExpandedCommand, bin_exes: &[String]) -> MissingTreeVerdict {
    let text = match recipe {
        ExpandedCommand::Shell { shell } => shell.clone(),
        ExpandedCommand::Argv { argv } => argv.join(" "),
    };
    let executables: BTreeSet<&str> = bin_exes.iter().map(String::as_str).collect();
    recipe_words(&text)
        .into_iter()
        .find(|word| executables.contains(word.as_str()))
        .map_or(MissingTreeVerdict::Run, |executable| {
            MissingTreeVerdict::OrphanedExeMissing { executable }
        })
}

fn recipe_words(recipe: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in recipe.chars() {
        if character.is_whitespace() || matches!(character, ';' | '|' | '&' | '(' | ')' | '<' | '>')
        {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if !matches!(character, '\'' | '"') {
            current.push(character);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_minimisation_is_exact_and_repo_tied_values_are_stripped() {
        let assembled = EnvMap::from([
            ("WT_LABEL".to_owned(), "repo".to_owned()),
            ("WT_ROOT".to_owned(), "/tree".to_owned()),
            ("WT_ACTIVATION".to_owned(), "secret-marker".to_owned()),
            ("PATH".to_owned(), "/tree/bin:/usr/bin".to_owned()),
            ("ALIAS".to_owned(), "alias".to_owned()),
            ("TASK_KEY".to_owned(), "task".to_owned()),
            ("PARENT_KEEP".to_owned(), "keep".to_owned()),
            ("PARENT_DROP".to_owned(), "drop".to_owned()),
        ]);
        let aliases = ["ALIAS".to_owned()];
        let task = ["TASK_KEY".to_owned()];
        let requested = ["PARENT_KEEP".to_owned(), "WT_ACTIVATION".to_owned()];

        let tree = minimise_env(&assembled, &aliases, &task, &requested, TiedTo::Tree);
        assert_eq!(
            tree.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "ALIAS",
                "PARENT_KEEP",
                "PATH",
                "TASK_KEY",
                "WT_LABEL",
                "WT_ROOT",
            ]
        );
        assert!(!tree.contains_key("PARENT_DROP"));
        assert!(!tree.contains_key("WT_ACTIVATION"));

        let repo = minimise_env(&assembled, &aliases, &task, &requested, TiedTo::Repo);
        assert_eq!(
            repo.keys().map(String::as_str).collect::<Vec<_>>(),
            ["ALIAS", "PARENT_KEEP", "TASK_KEY", "WT_LABEL"]
        );
    }

    #[test]
    fn missing_tree_word_test_uses_only_the_recipe_about_to_run() {
        let bins = ["orbit".to_owned(), "helper".to_owned()];
        let safe = ExpandedCommand::Shell {
            shell: "docker stop orbit-server".to_owned(),
        };
        assert_eq!(missing_tree_verdict(&safe, &bins), MissingTreeVerdict::Run);

        let blocked = ExpandedCommand::Shell {
            shell: "probe && 'orbit' server stop".to_owned(),
        };
        assert_eq!(
            missing_tree_verdict(&blocked, &bins),
            MissingTreeVerdict::OrphanedExeMissing {
                executable: "orbit".to_owned()
            }
        );

        let other_recipe = ExpandedCommand::Argv {
            argv: vec!["docker".to_owned(), "stop".to_owned()],
        };
        assert_eq!(
            missing_tree_verdict(&other_recipe, &["orbit".to_owned()]),
            MissingTreeVerdict::Run
        );
    }
}
