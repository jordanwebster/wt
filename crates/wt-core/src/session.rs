/// Derives the persisted tmux identity. Collision detection belongs to the
/// coordinate transaction because it compares live addresses and tombstones.
pub fn name(label: &str, tree: &str) -> String {
    let target = if tree == "canonical" {
        label.to_owned()
    } else {
        format!("{label}/{tree}")
    };
    target.replace(['.', ':'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_is_the_sanitised_display_target() {
        assert_eq!(name("a.b", "canonical"), "a_b");
        assert_eq!(name("a.b", "feature.x"), "a_b/feature_x");
        assert_eq!(name("repo", "feature-x"), "repo/feature-x");
    }
}
