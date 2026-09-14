mod common;

use common::Harness;

const FIXTURES: [(&str, &str); 5] = [
    ("orbit", include_str!("../spec/acceptance/orbit.wt.toml")),
    (
        "orbitcloud",
        include_str!("../spec/acceptance/orbitcloud.wt.toml"),
    ),
    (
        "orbittool",
        include_str!("../spec/acceptance/orbittool.wt.toml"),
    ),
    (
        "orbitapp",
        include_str!("../spec/acceptance/orbitapp.wt.toml"),
    ),
    (
        "orbitmono",
        include_str!("../spec/acceptance/orbitmono.wt.toml"),
    ),
];

#[test]
fn target_configurations_render_without_running_their_recipes() {
    for (label, source) in FIXTURES {
        let harness = Harness::new();
        common::write(
            &harness.home.join("config.toml"),
            "[session]\nbackend='none'\n",
        );
        let repo = harness.repo(label, source);
        harness.register(&repo);
        let rendered = harness.json(&["config", label]);
        assert_eq!(rendered["data"]["target"], label);
        assert_eq!(rendered["ok"], true);
    }
}

#[test]
fn pooled_device_fixture_uses_frozen_names_keep_and_idle_only() {
    let mobile = include_str!("../spec/acceptance/orbit.wt.toml");
    assert!(mobile.contains("$WT_SELF"));
    assert_eq!(mobile.matches("keep = \"2m\"").count(), 2);
    assert_eq!(mobile.matches("idle = \"30m\"").count(), 2);
    assert_eq!(mobile.matches("wait = \"45m\"").count(), 2);
    for (label, source) in FIXTURES {
        assert!(!source.contains("reset"), "{label} contains removed reset");
    }
}
