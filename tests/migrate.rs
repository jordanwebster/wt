//! Importing trees from wt 0.5's registry.

mod common;

use std::fs;

use common::Fixture;

#[test]
fn imports_trees_with_their_ports_and_clears_old_artifacts() {
    let fx = Fixture::new();
    let tree = fx.worktree("desktopapp");
    let gone = fx.root.join("trees/gone");
    fs::create_dir_all(tree.join(".wt/shims")).unwrap();
    std::os::unix::fs::symlink("/usr/local/bin/wt", tree.join(".wt/shims/amux")).unwrap();
    fs::write(tree.join(".wt/tree_id"), "abc").unwrap();
    fs::create_dir_all(tree.join(".wt/amux")).unwrap();
    fs::write(tree.join(".wt/amux/config.toml"), "kept").unwrap();

    // The shape wt 0.5 wrote, trimmed to the fields that matter.
    let registry = serde_json::json!({
        "schema": 1,
        "labels": { "app": { "path": fx.repo, "default_branch": "main" } },
        "trees": [
            { "label": "app", "name": "canonical", "canonical": true, "path": fx.repo, "slot": 2,
              "geometry": { "base": 20000, "stride": 16, "port_base": 20032 },
              "ports": { "postgres": 3, "server": 4 } },
            { "label": "app", "name": "desktopapp", "canonical": false, "path": tree, "slot": 95,
              "geometry": { "base": 20000, "stride": 16, "port_base": 21520 }, "ports": {} },
            { "label": "app", "name": "gone", "canonical": false, "path": gone, "slot": 96,
              "geometry": { "base": 20000, "stride": 16, "port_base": 21536 }, "ports": {} }
        ],
        "tombstones": []
    });
    fs::create_dir_all(&fx.home).unwrap();
    fs::write(fx.home.join("registry.json"), registry.to_string()).unwrap();

    let wt = fx.wt();
    assert!(wt.needs_import());
    let imported = wt.import_legacy().unwrap();
    assert_eq!(imported.trees, ["app/canonical", "app/desktopapp"]);
    assert_eq!(imported.warnings.len(), 1, "{:?}", imported.warnings);
    assert!(imported.warnings[0].contains("app/gone"));
    assert!(!wt.needs_import());

    let main = wt.find(Some("app/canonical"), &fx.root).unwrap();
    assert!(main.is_main());
    assert_eq!(main.ports()["postgres"], 20000 + 2 * 16 + 3);
    assert_eq!(main.ports()["server"], 20000 + 2 * 16 + 4);
    let desktop = wt.find(Some("app/desktopapp"), &fx.root).unwrap();
    assert_eq!(desktop.slot, 95);
    assert_eq!(desktop.main, fx.repo);

    assert!(!tree.join(".wt/shims").exists());
    assert!(!tree.join(".wt/tree_id").exists());
    assert_eq!(
        fs::read_to_string(tree.join(".wt/amux/config.toml")).unwrap(),
        "kept"
    );

    // A port declared later goes after the imported offsets.
    fx.config("ports = [\"postgres\", \"server\", \"web\"]\n");
    let env = main.env().unwrap();
    assert_eq!(env.vars["WT_PORT_WEB"], (20000 + 2 * 16 + 5).to_string());

    assert!(wt.import_legacy().is_err());
}

#[test]
fn the_cli_asks_for_the_import_first() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.home).unwrap();
    fs::write(
        fx.home.join("registry.json"),
        r#"{"labels": {}, "trees": []}"#,
    )
    .unwrap();
    let output = fx.cli(&fx.repo, &["ls"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("wt migrate"));
    let output = fx.cli(&fx.repo, &["migrate"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fx.cli(&fx.repo, &["ls"]).status.success());
}
