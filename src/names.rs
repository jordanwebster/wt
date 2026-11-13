//! Names derived from a tree's identity, for use in templates.

/// Lower-case ASCII letters and digits, with every run of anything else
/// collapsed to one underscore: `Fix-Scroll 2` becomes `fix_scroll_2`.
pub fn snake(value: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if separator && !out.is_empty() {
                out.push('_');
            }
            separator = false;
            out.push(ch);
        } else {
            separator = true;
        }
    }
    if out.is_empty() { "x".to_owned() } else { out }
}

/// A short name that is unique per tree across repositories: the snake-cased
/// `label_name` cut to 22 characters, plus a hash of the whole of it so that
/// two long names with the same prefix stay distinct. Existing trees derive
/// daemon and host names from this, so the algorithm must not change.
pub fn short(label: &str, name: &str) -> String {
    let full = format!("{}_{}", snake(label), snake(name));
    let hash = blake3::hash(full.as_bytes()).to_hex();
    format!(
        "{}_{}",
        full.chars().take(22).collect::<String>(),
        &hash[..8]
    )
}

/// Labels, tree names, port names, pool names and `[bin]` names share one
/// rule, so each can appear in a path, an environment variable name and a
/// shell word without quoting.
pub fn valid(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['.', '-'])
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// `metro-port` → `METRO_PORT`, for `WT_PORT_<NAME>` and `WT_LEASE_<POOL>`.
pub fn env_suffix(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_collapses_separators() {
        assert_eq!(snake("Fix-Scroll  2"), "fix_scroll_2");
        assert_eq!(snake("--"), "x");
    }

    #[test]
    fn short_matches_the_previous_algorithm() {
        // Values rendered by wt 0.5, so existing trees keep their names.
        assert_eq!(short("amux", "desktopapp"), "amux_desktopapp_9353b863");
        assert_eq!(short("amux", "canonical"), "amux_canonical_48b140d8");
    }

    #[test]
    fn valid_names() {
        assert!(valid("fix-scroll_2"));
        assert!(!valid("a/b"));
        assert!(!valid(".hidden"));
        assert!(!valid(""));
    }
}
