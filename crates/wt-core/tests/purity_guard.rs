#![forbid(unsafe_code)]
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::path::Path;

fn rust_sources(root: &Path, paths: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, paths);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
}

fn has_forbidden_std_path(source: &str, module: &str) -> bool {
    if source.contains(&format!("std::{module}")) {
        return true;
    }
    let mut rest = source;
    while let Some(start) = rest.find("std::{") {
        rest = &rest[start + "std::{".len()..];
        let statement = rest
            .split_once(';')
            .map_or(rest, |(statement, _)| statement);
        if statement
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|identifier| identifier == module)
        {
            return true;
        }
    }
    false
}

#[test]
fn purity_barrier_covers_demonstrated_bypasses_and_source_is_clean() {
    let policy = include_str!("../clippy.toml");
    let rejected = [
        "std::env::home_dir",
        "std::os::unix::fs::symlink",
        "std::println",
        "std::eprintln",
        "std::print",
        "std::dbg",
    ];
    for path in rejected {
        assert!(
            policy.contains(path),
            "purity policy does not reject {path}"
        );
    }

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    rust_sources(&source_root, &mut paths);
    assert!(!paths.is_empty());
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        for module in ["fs", "process", "env", "net", "thread", "os"] {
            assert!(
                !has_forbidden_std_path(&source, module),
                "wt-core source bypasses purity through std::{module} in {}",
                path.display()
            );
        }
        for clock in ["SystemTime", "Instant"] {
            assert!(
                !source.contains(clock),
                "wt-core source observes time through {clock} in {}",
                path.display()
            );
        }
    }
    let lib = std::fs::read_to_string(source_root.join("lib.rs")).unwrap();
    assert!(lib.contains("#![forbid(unsafe_code)]"));
}
