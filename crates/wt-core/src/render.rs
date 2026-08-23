use serde::{Deserialize, Serialize};

use crate::{CoreError, ExitClass};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observed {
    Tracked,
    Symlink,
    Absent,
    Regular(Vec<u8>),
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenderRecord {
    pub hash: Option<String>,
}

pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Write,
    Unchanged,
}

/// Evaluates SPEC §8.3 in numbered order; earlier ownership refusals win.
pub fn decide(
    observed: &Observed,
    record: Option<&RenderRecord>,
    new_bytes: &[u8],
) -> Result<Decision, CoreError> {
    match observed {
        Observed::Tracked => {
            return Err(render_error_with_remedy(
                "RENDER_ONTO_TRACKED",
                "target is tracked by git",
                "untrack `<path>`, or disable it in `<tree>/.wt/config.toml` with `files.\"<path>\" = false`",
            ))
        }
        Observed::Symlink => {
            return Err(render_error_with_remedy(
                "RENDER_ONTO_SYMLINK",
                "target is a symlink",
                "run `rm <path>` so wt can render it, or disable it in `<tree>/.wt/config.toml` with `files.\"<path>\" = false`",
            ))
        }
        Observed::Absent => return Ok(Decision::Write),
        Observed::Regular(bytes) => {
            let hash = blake3::hash(bytes).to_hex().to_string();
            if let Some(record) = record {
                if record.hash.as_deref() == Some(&hash) {
                    return if bytes == new_bytes {
                        Ok(Decision::Unchanged)
                    } else {
                        Ok(Decision::Write)
                    };
                }
            }
        }
        Observed::Other => {}
    }
    Err(render_error_with_remedy(
        "RENDER_ONTO_USER_FILE",
        "target is not owned by wt",
        "run `rm <path>` so wt can render it again, or disable it in `<tree>/.wt/config.toml` with `files.\"<path>\" = false`",
    ))
}

fn render_error_with_remedy(code: &str, message: &str, remedy: &str) -> CoreError {
    CoreError::new(ExitClass::State, code, message, remedy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    #[test]
    fn numbered_table_is_exhaustive_and_ordered() {
        let prior = b"old".to_vec();
        let new = b"new";
        let record = RenderRecord {
            hash: Some(hash(&prior)),
        };
        assert_eq!(
            decide(&Observed::Tracked, Some(&record), new)
                .unwrap_err()
                .code
                .0,
            "RENDER_ONTO_TRACKED"
        );
        assert_eq!(
            decide(&Observed::Symlink, Some(&record), new)
                .unwrap_err()
                .code
                .0,
            "RENDER_ONTO_SYMLINK"
        );
        assert_eq!(
            decide(&Observed::Absent, None, new).unwrap(),
            Decision::Write
        );
        assert_eq!(
            decide(&Observed::Regular(prior.clone()), Some(&record), new).unwrap(),
            Decision::Write
        );
        assert_eq!(
            decide(&Observed::Regular(prior.clone()), Some(&record), &prior).unwrap(),
            Decision::Unchanged
        );
        assert_eq!(
            decide(&Observed::Regular(b"edited".to_vec()), Some(&record), new)
                .unwrap_err()
                .code
                .0,
            "RENDER_ONTO_USER_FILE"
        );
    }
}
