use std::collections::BTreeSet;

#[test]
fn manifest_dependencies_stay_within_the_pure_core_allowlist() {
    let manifest: toml::Table = include_str!("../Cargo.toml").parse().unwrap();
    let actual: BTreeSet<&str> = manifest["dependencies"]
        .as_table()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let allowed = BTreeSet::from([
        "serde",
        "serde_json",
        "toml",
        "blake3",
        "indexmap",
        "thiserror",
    ]);
    assert_eq!(actual, allowed);

    let actual_dev: BTreeSet<&str> = manifest["dev-dependencies"]
        .as_table()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let allowed_dev = BTreeSet::from(["proptest", "insta"]);
    assert_eq!(actual_dev, allowed_dev);
}
