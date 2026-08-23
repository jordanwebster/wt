use std::collections::BTreeSet;

use crate::{model::PortMap, model::PortName, CoreError, ExitClass};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendResult {
    pub ports: PortMap,
    pub appended: Vec<PortName>,
}

/// Appends newly declared names without making declaration order semantic.
/// Existing indices remain reserved even when their names leave the config.
pub fn append(
    recorded: &PortMap,
    configured: &[PortName],
    stride: u8,
) -> Result<AppendResult, CoreError> {
    if configured.len() > usize::from(stride) {
        return Err(CoreError::new(
            ExitClass::State,
            "CONFIG_INVALID",
            "declared ports exceed the frozen stride",
            "declare fewer ports or re-create the tree with a larger stride",
        ));
    }

    let mut used = BTreeSet::new();
    for index in recorded.values().copied() {
        if index >= stride || !used.insert(index) {
            return Err(CoreError::new(
                ExitClass::State,
                "REGISTRY_CORRUPT",
                "the recorded ports map contains an invalid or duplicate index",
                "delete the corrupt registry and re-register the affected checkouts",
            ));
        }
    }

    let configured: BTreeSet<_> = configured.iter().cloned().collect();
    let mut ports = recorded.clone();
    let mut appended = Vec::new();
    for name in configured {
        if ports.contains_key(&name) {
            continue;
        }
        let Some(index) = (0..stride).find(|index| !used.contains(index)) else {
            return Err(CoreError::new(
                ExitClass::Conflict,
                "PORTS_EXHAUSTED",
                "the tree has no unused port index",
                "run `wt remove` then `wt new`, or raise ports.stride for future trees",
            ));
        };
        used.insert(index);
        ports.insert(name.clone(), index);
        appended.push(name);
    }
    Ok(AppendResult { ports, appended })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<PortName> {
        values
            .iter()
            .map(|value| PortName::new(*value).unwrap())
            .collect()
    }

    #[test]
    fn append_is_reorder_proof_and_never_reuses_removed_names() {
        let initial = append(&PortMap::new(), &names(&["http", "admin"]), 4).unwrap();
        let reordered = append(&initial.ports, &names(&["admin", "http"]), 4).unwrap();
        assert!(reordered.appended.is_empty());
        assert_eq!(reordered.ports, initial.ports);

        let removed = append(&initial.ports, &names(&["admin"]), 4).unwrap();
        let extended = append(&removed.ports, &names(&["metrics", "admin"]), 4).unwrap();
        assert_eq!(extended.ports[&PortName::new("http").unwrap()], 1);
        assert_eq!(extended.ports[&PortName::new("metrics").unwrap()], 2);
    }

    #[test]
    fn exhaustion_and_corrupt_recorded_indices_are_distinct() {
        let full = append(&PortMap::new(), &names(&["a", "b"]), 2)
            .unwrap()
            .ports;
        assert_eq!(
            append(&full, &names(&["c"]), 2).unwrap_err().code.0,
            "PORTS_EXHAUSTED"
        );
        let corrupt = PortMap::from([
            (PortName::new("a").unwrap(), 0),
            (PortName::new("b").unwrap(), 0),
        ]);
        assert_eq!(
            append(&corrupt, &[], 2).unwrap_err().code.0,
            "REGISTRY_CORRUPT"
        );
    }
}
