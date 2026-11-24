//! Per-tree parameters: ports, environment, rendered files, `[bin]`
//! wrappers, copied inputs, and finding trees.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use common::{Fixture, git, make, stdout};
use wt::Init;

const CONFIG: &str = r#"
ports = ["server", "web"]

[env]
URL = "http://localhost:{{ports.server}}/{{name_snake}}"

[files.".wt/app.toml"]
content = "url = \"{{env.URL}}\"\nroot = \"{{root}}\"\n"

[bin]
tool = "build/tool"
"#;

#[test]
fn init_gives_each_tree_its_own_ports_and_environment() {
    let fx = Fixture::new();
    fx.config(CONFIG);
    let wt = fx.wt();

    let main = wt.init(&fx.repo, Init::default()).unwrap();
    assert_eq!(main.id(), "app/main");
    assert!(main.is_main());

    let path = fx.worktree("fix-scroll");
    let tree = wt.init(&path, Init::default()).unwrap();
    assert_eq!(tree.id(), "app/fix-scroll");
    assert_eq!(tree.main, fx.repo);
    assert_ne!(tree.slot, main.slot);

    let env = tree.env().unwrap();
    let server = tree.ports()["server"];
    assert_eq!(tree.ports()["web"], server + 1);
    assert_eq!(env.vars["WT_TREE"], "app/fix-scroll");
    assert_eq!(env.vars["WT_ROOT"], path.display().to_string());
    assert_eq!(env.vars["WT_MAIN"], fx.repo.display().to_string());
    assert_eq!(env.vars["WT_PORT_SERVER"], server.to_string());
    assert_eq!(
        env.vars["URL"],
        format!("http://localhost:{server}/fix_scroll")
    );
    assert_eq!(env.path_prefix, [path.join(".wt/bin")]);

    let rendered = fs::read_to_string(path.join(".wt/app.toml")).unwrap();
    assert_eq!(
        rendered,
        format!(
            "url = \"http://localhost:{server}/fix_scroll\"\nroot = \"{}\"\n",
            path.display()
        )
    );
}

#[test]
fn init_again_keeps_name_and_ports_and_ports_are_append_only() {
    let fx = Fixture::new();
    fx.config("ports = [\"a\", \"b\"]\n");
    let wt = fx.wt();
    let path = fx.worktree("t");
    let first = wt.init(&path, Init::default()).unwrap();
    let again = wt.init(&path, Init::default()).unwrap();
    assert_eq!(first.slot, again.slot);
    assert_eq!(first.ports(), again.ports());

    // The branch drops `a` and adds `c`: `b` keeps its port and `c` gets a
    // new one rather than `a`'s.
    fs::write(path.join(".wt.toml"), "ports = [\"b\", \"c\"]\n").unwrap();
    let env = wt.find(Some("app/t"), &fx.root).unwrap().env().unwrap();
    let base = first.ports()["a"];
    assert_eq!(env.vars["WT_PORT_B"], (base + 1).to_string());
    assert_eq!(env.vars["WT_PORT_C"], (base + 2).to_string());
}

#[test]
fn bin_wrappers_run_the_trees_build_or_say_why_not() {
    let fx = Fixture::new();
    fx.config(CONFIG);
    let path = fx.worktree("t");
    let tree = fx.wt().init(&path, Init::default()).unwrap();
    let wrapper = path.join(".wt/bin/tool");
    assert_eq!(
        fs::metadata(&wrapper).unwrap().permissions().mode() & 0o777,
        0o755
    );

    let output = Command::new(&wrapper).output().unwrap();
    assert_eq!(output.status.code(), Some(127));
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("not ready in app/t"), "{message}");
    assert!(message.contains("missing"), "{message}");

    fs::create_dir_all(path.join(".wt")).unwrap();
    fs::write(path.join(".wt/create.status"), "running").unwrap();
    let output = Command::new(&wrapper).output().unwrap();
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("still running"), "{message}");

    fs::create_dir_all(path.join("build")).unwrap();
    fs::write(path.join("build/tool"), "#!/bin/sh\necho built \"$@\"\n").unwrap();
    fs::set_permissions(path.join("build/tool"), fs::Permissions::from_mode(0o755)).unwrap();
    // A binary that is there while create still runs isn't trusted.
    let output = Command::new(&wrapper).arg("x").output().unwrap();
    assert_eq!(output.status.code(), Some(127));
    fs::write(path.join(".wt/create.status"), "2").unwrap();
    let output = Command::new(&wrapper).arg("x").output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("create hook failed"));
    fs::write(path.join(".wt/create.status"), "0").unwrap();
    let output = Command::new(&wrapper).arg("x").output().unwrap();
    assert_eq!(stdout(&output), "built x\n");

    // Removing the entry removes the wrapper, but not other files there.
    fs::write(path.join(".wt/bin/other"), "#!/bin/sh\n").unwrap();
    fs::write(path.join(".wt.toml"), "").unwrap();
    tree.env().unwrap();
    assert!(!wrapper.exists());
    assert!(path.join(".wt/bin/other").exists());
}

#[test]
fn a_binary_built_into_the_wrapper_directory_is_never_overwritten() {
    let fx = Fixture::new();
    fx.config("[bin]\ntool = \".wt/bin/tool\"\n");
    let path = fx.worktree("t");
    let tree = fx.wt().init(&path, Init::default()).unwrap();
    let binary = path.join(".wt/bin/tool");

    // A wrapper an older wt wrote there, which execs itself, is removed.
    fs::create_dir_all(path.join(".wt/bin")).unwrap();
    fs::write(&binary, "#!/bin/sh\n# wt:bin\nexec \"$0\" \"$@\"\n").unwrap();
    tree.env().unwrap();
    assert!(!binary.exists());

    // The tree's build, once there, is left alone.
    fs::write(&binary, "#!/bin/sh\necho built\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    tree.env().unwrap();
    assert_eq!(stdout(&Command::new(&binary).output().unwrap()), "built\n");
}

#[test]
fn rendered_paths_are_excluded_from_git() {
    let fx = Fixture::new();
    fx.config("[files.\"generated.json\"]\ncontent = \"{}\"\n[bin]\ntool = \"x\"\n");
    let path = fx.worktree("t");
    fx.wt().init(&path, Init::default()).unwrap();
    assert!(path.join("generated.json").exists());
    assert_eq!(git(&path, &["status", "--porcelain"]), "");
}

#[test]
fn copy_brings_files_from_the_main_checkout_once() {
    let fx = Fixture::new();
    let outside = fx.root.join("outside.txt");
    fs::write(&outside, "outside").unwrap();
    fx.config(&format!(
        "copy = [\"local/settings.json\", \"cache\", {{ from = \"{}\", to = \".wt/outside.txt\" }}]\n",
        outside.display()
    ));
    fs::create_dir_all(fx.repo.join("local")).unwrap();
    fs::write(fx.repo.join("local/settings.json"), "mine").unwrap();
    fs::create_dir_all(fx.repo.join("cache/deep")).unwrap();
    fs::write(fx.repo.join("cache/deep/blob"), "blob").unwrap();

    let path = fx.worktree("t");
    fs::create_dir_all(path.join("local")).unwrap();
    fs::write(path.join("local/settings.json"), "already here").unwrap();
    fx.wt()
        .init(&path, Init::default())
        .unwrap()
        .copy_inputs()
        .unwrap();

    assert_eq!(
        fs::read_to_string(path.join("local/settings.json")).unwrap(),
        "already here"
    );
    assert_eq!(
        fs::read_to_string(path.join("cache/deep/blob")).unwrap(),
        "blob"
    );
    assert_eq!(
        fs::read_to_string(path.join(".wt/outside.txt")).unwrap(),
        "outside"
    );
}

#[test]
fn the_user_layer_replaces_the_repository_config() {
    let fx = Fixture::new();
    fx.config("[env]\nFROM = \"repo\"\n");
    fs::create_dir_all(fx.home.join("repos/app")).unwrap();
    fs::write(
        fx.home.join("repos/app/wt.toml"),
        "[env]\nOTHER = \"user\"\n",
    )
    .unwrap();
    let tree = fx.wt().init(&fx.repo, Init::default()).unwrap();
    let env = tree.env().unwrap();
    assert_eq!(env.vars.get("OTHER").map(String::as_str), Some("user"));
    assert!(!env.vars.contains_key("FROM"));
}

#[test]
fn config_errors_name_the_file() {
    let fx = Fixture::new();
    fx.config("[hooks]\nafter_new = \"x\"\n");
    let error = fx
        .wt()
        .init(&fx.repo, Init::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains(".wt.toml"), "{error}");
    assert!(error.contains("lifecycle.create"), "{error}");
}

#[test]
fn find_by_id_name_or_directory() {
    let fx = Fixture::new();
    let wt = fx.wt();
    wt.init(&fx.repo, Init::default()).unwrap();
    let path = fx.worktree("t");
    wt.init(&path, Init::default()).unwrap();

    assert_eq!(wt.find(Some("app/t"), &fx.root).unwrap().root, path);
    assert_eq!(wt.find(Some("t"), &fx.root).unwrap().root, path);
    fs::create_dir_all(path.join("sub")).unwrap();
    assert_eq!(wt.find(None, &path.join("sub")).unwrap().root, path);
    assert_eq!(wt.find(None, &fx.repo).unwrap().id(), "app/main");
    assert!(wt.find(None, &fx.root).is_err());
    assert!(wt.find(Some("nope"), &fx.root).is_err());
}

#[test]
fn a_label_belongs_to_one_repository() {
    let fx = Fixture::new();
    let wt = fx.wt();
    wt.init(&fx.repo, Init::default()).unwrap();
    let other = fx.root.join("other/app");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    let error = wt.init(&other, Init::default()).unwrap_err().to_string();
    assert!(error.contains("--label"), "{error}");
    let tree = wt
        .init(
            &other,
            Init {
                label: Some("other".into()),
                name: None,
            },
        )
        .unwrap();
    assert_eq!(tree.id(), "other/main");
}

#[test]
fn exec_runs_in_the_tree_and_keeps_a_subdirectory() {
    let fx = Fixture::new();
    fx.config(CONFIG);
    let path = fx.worktree("t");
    fx.wt().init(&path, Init::default()).unwrap();
    fs::create_dir_all(path.join("sub")).unwrap();

    let script = "echo $WT_TREE; pwd; command -v tool";
    let inside = stdout(&fx.cli(&path.join("sub"), &["exec", "--", "sh", "-c", script]));
    assert_eq!(
        inside,
        format!(
            "app/t\n{}\n{}\n",
            path.join("sub").display(),
            path.join(".wt/bin/tool").display()
        )
    );

    let outside = stdout(&fx.cli(&fx.root, &["exec", "t", "--", "pwd"]));
    assert_eq!(outside, format!("{}\n", path.display()));

    let failed = fx.cli(&fx.root, &["exec", "t", "--", "sh", "-c", "exit 3"]);
    assert_eq!(failed.status.code(), Some(3));
}

#[test]
fn env_prints_shell_exports() {
    let fx = Fixture::new();
    fx.config(CONFIG);
    let path = fx.worktree("t");
    fx.wt().init(&path, Init::default()).unwrap();
    let exports = stdout(&fx.cli(&path, &["env"]));
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{exports}\necho \"$WT_TREE $URL\"; command -v tool"
        ))
        .output()
        .unwrap();
    let printed = stdout(&output);
    assert!(printed.starts_with("app/t http://localhost:"), "{printed}");
    assert!(printed.ends_with(".wt/bin/tool\n"), "{printed}");
}

#[test]
fn port_blocks_skip_ports_in_use_on_ipv6_localhost() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.home).unwrap();
    fs::write(
        fx.home.join("config.toml"),
        "[ports]\nbase = 47310\nstride = 2\n",
    )
    .unwrap();
    let Ok(_listener) = std::net::TcpListener::bind(("::1", 47310)) else {
        return; // no IPv6 on this machine
    };
    fx.config("ports = [\"a\"]\n");
    let tree = fx.wt().init(&fx.repo, Init::default()).unwrap();
    assert_ne!(tree.slot, 0);
}

#[test]
fn a_colon_in_the_path_does_not_break_entering_the_tree() {
    let fx = Fixture::new();
    let repo = fx.root.join("my:repos/app");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let tree = fx.wt().init(&repo, Init::default()).unwrap();
    let env = tree.env().unwrap();
    assert!(env.to_shell().contains("my:repos"));
    stdout(&fx.cli(&repo, &["exec", "--", "true"]));
}

#[test]
fn writes_wait_for_a_pending_import() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.home).unwrap();
    fs::write(
        fx.home.join("registry.json"),
        r#"{"labels": {}, "trees": []}"#,
    )
    .unwrap();
    let error = fx
        .wt()
        .init(&fx.repo, Init::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("wt migrate"), "{error}");
}

#[test]
fn unknown_config_keys_are_ignored_with_a_warning_naming_the_file() {
    let fx = Fixture::new();
    fx.config("future_key = 1\n[lifecycle]\nfuture_hook = \"x\"\n[env]\nA = \"1\"\n");
    let tree = make(&fx.wt(), &fx.repo, wt::PlanRequest::named("t"))
        .unwrap()
        .tree;
    let output = fx.cli(&tree.root, &["exec", "--", "sh", "-c", "echo $A"]);
    assert_eq!(stdout(&output), "1\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown key `future_key`"), "{stderr}");
    assert!(
        stderr.contains("unknown key `lifecycle.future_hook`"),
        "{stderr}"
    );
    assert!(stderr.contains(".wt.toml"), "{stderr}");
    let config = tree.config().unwrap();
    assert_eq!(config.warnings.len(), 2, "{:?}", config.warnings);

    // A wrong value is still an error.
    fs::write(tree.root.join(".wt.toml"), "ports = \"web\"\n").unwrap();
    assert!(tree.config().is_err());

    // So is an unknown key in the machine settings, as a warning too.
    fs::write(fx.home.join("config.toml"), "future = true\n").unwrap();
    let wt = fx.wt();
    assert!(wt.settings().warnings[0].contains("unknown key `future`"));
}

#[test]
fn a_kept_file_is_written_when_missing_and_never_overwritten() {
    let fx = Fixture::new();
    fx.config(
        "[files.\"state.toml\"]\ncontent = \"tree = \\\"{{name}}\\\"\\n\"\nkeep = true\n\
         [files.\"rendered.txt\"]\ncontent = \"{{name}}\"\n",
    );
    let tree = make(&fx.wt(), &fx.repo, wt::PlanRequest::named("t"))
        .unwrap()
        .tree;
    let state = tree.root.join("state.toml");
    let rendered = tree.root.join("rendered.txt");
    assert_eq!(fs::read_to_string(&state).unwrap(), "tree = \"t\"\n");
    assert!(tree.config().unwrap().warnings.is_empty());

    fs::write(&state, "tree = \"edited\"\n").unwrap();
    fs::write(&rendered, "edited").unwrap();
    tree.env().unwrap();
    assert_eq!(fs::read_to_string(&state).unwrap(), "tree = \"edited\"\n");
    assert_eq!(fs::read_to_string(&rendered).unwrap(), "t");

    fs::remove_file(&state).unwrap();
    stdout(&fx.cli(&tree.root, &["exec", "--", "true"]));
    assert_eq!(fs::read_to_string(&state).unwrap(), "tree = \"t\"\n");
}

#[test]
fn a_kept_file_never_replaces_one_created_while_entering() {
    use std::os::unix::fs::MetadataExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let fx = Fixture::new();
    fx.config("[files.\"state.toml\"]\ncontent = \"rendered\"\nkeep = true\n");
    let tree = make(&fx.wt(), &fx.repo, wt::PlanRequest::named("t"))
        .unwrap()
        .tree;
    let path = tree.root.join("state.toml");
    let done = Arc::new(AtomicBool::new(false));
    let entering = {
        let (tree, done) = (tree.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                tree.env().unwrap();
            }
        })
    };
    // An application that makes the file itself whenever it is missing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut clobbered = None;
    while clobbered.is_none() && std::time::Instant::now() < deadline {
        let _ = fs::remove_file(&path);
        let Ok(file) = fs::File::options().write(true).create_new(true).open(&path) else {
            continue;
        };
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mine = file.metadata().unwrap().ino();
        if fs::metadata(&path).map(|there| there.ino()).ok() != Some(mine) {
            clobbered = Some(fs::read_to_string(&path).unwrap_or_default());
        }
    }
    done.store(true, Ordering::SeqCst);
    entering.join().unwrap();
    assert_eq!(clobbered, None, "wt replaced a file the application made");
}
