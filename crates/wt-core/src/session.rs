/// Derives the persisted tmux identity. Collision detection belongs to the
/// coordinate transaction because it compares live addresses and tombstones.
pub fn name(label: &str, tree: &str) -> String {
    let san = |value: &str, max: usize| {
        value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                    ch
                } else {
                    '_'
                }
            })
            .take(max)
            .collect::<String>()
    };
    let hash = blake3::hash(format!("{label}/{tree}").as_bytes()).to_hex();
    format!("wt_{}_{}_{}", san(label, 16), san(tree, 24), &hash[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_and_uses_the_fixed_hash_width() {
        let value = name("a.b", "feature/x");
        assert!(value.starts_with("wt_a_b_feature_x_"));
        assert_eq!(value.rsplit('_').next().unwrap().len(), 8);
        assert_eq!(value, name("a.b", "feature/x"));
    }
}
