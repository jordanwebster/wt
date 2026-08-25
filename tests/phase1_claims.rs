mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{git, proof_capture, write, write_executable, Harness};
use wt_sys::proc::{self, CommandRequest, ProcessOutput};

const OWNED_CONFIG: &str = r#"
bin = ["target/debug"]
commands = ["orbit"]

[task.build]
run = "mkdir -p target/debug && ln -sfn ../../fixture/orbit target/debug/orbit"
"#;

fn owned_fixture(harness: &Harness) -> PathBuf {
    let repo = harness.repo("repo", OWNED_CONFIG);
    write_executable(
        &repo.join("fixture/orbit"),
        "#!/bin/sh\nprintf 'TREE pid=%s parent=%s args=%s env=%s\\n' \"$$\" \"$PPID\" \"$*\" \"${OWNED_ENV-}\"\n",
    );
    git(&repo, &["add", "fixture/orbit"]);
    git(&repo, &["commit", "-qm", "add owned command fixture"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    write_executable(
        &harness.shims.join("orbit"),
        "#!/bin/sh\nprintf 'installed %s\\n' \"$*\" >> \"$WT_SHIM_STATE/installed.log\"\nprintf 'INSTALLED\\n'\nexit 99\n",
    );
    write_executable(
        &harness.shims.join("galaxy"),
        "#!/bin/sh\nprintf 'GALAXY %s\\n' \"$*\"\n",
    );
    wt_sys::fsx::replace_symlink(
        &harness.shims.join("wt"),
        Path::new(env!("CARGO_BIN_EXE_wt")),
    )
    .unwrap();
    harness.register(&repo);
    repo
}

fn assembled(harness: &Harness, target: &str) -> BTreeMap<String, String> {
    harness.json(&["env", target])["data"]["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
        .collect()
}

fn capture_program(program: &str, args: &[&str], env: BTreeMap<String, String>) -> ProcessOutput {
    let mut request = CommandRequest::new(program);
    request.args = proc::os_args(args);
    request.clear_env = true;
    request.env = env;
    proc::capture(&request, Duration::from_secs(10)).unwrap()
}

fn text(output: &ProcessOutput) -> String {
    format!(
        "exit={}\nstdout:\n{}stderr:\n{}",
        output.mapped_exit(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn owned_command_refuses_then_executes_and_unclaimed_names_resolve_normally() {
    let harness = Harness::new();
    let repo = owned_fixture(&harness);
    let mut env = assembled(&harness, "repo");
    env.insert("OWNED_ENV".to_owned(), "preserved".to_owned());

    let refused = capture_program("orbit", &["first", "argument"], env.clone());
    assert_eq!(refused.child.code, Some(5));
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(refusal.contains("COMMAND_NOT_BUILT"));
    assert!(refusal.contains("tree `repo`"));
    assert!(refusal.contains(&repo.join("target/debug").to_string_lossy().to_string()));
    assert!(refusal.contains("wt build repo"));
    assert!(refusal.contains(&harness.shims.join("orbit").to_string_lossy().to_string()));
    assert!(!harness.shim_state.join("installed.log").exists());

    let bare_wt = harness
        .wt()
        .args(["exec", "repo", "--", "sh", "-c", "wt list --json"])
        .output()
        .unwrap();
    assert!(
        bare_wt.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&bare_wt.stdout),
        String::from_utf8_lossy(&bare_wt.stderr)
    );
    let _: serde_json::Value = serde_json::from_slice(&bare_wt.stdout).unwrap();

    let prepended = harness.root.join("prepended");
    wt_sys::fsx::create_private_dir(&prepended).unwrap();
    let mut prepended_env = env.clone();
    prepended_env.insert(
        "PATH".to_owned(),
        format!("{}:{}", prepended.display(), prepended_env["PATH"]),
    );
    let still_owned = capture_program("orbit", &["after-prepend"], prepended_env);
    assert_eq!(still_owned.child.code, Some(5));
    assert!(String::from_utf8_lossy(&still_owned.stderr).contains("COMMAND_NOT_BUILT"));

    let unclaimed = capture_program("galaxy", &["untouched"], env.clone());
    assert_eq!(unclaimed.child.code, Some(0));
    assert_eq!(
        String::from_utf8_lossy(&unclaimed.stdout),
        "GALAXY untouched\n"
    );

    harness.json(&["build", "repo"]);
    let built = capture_program("orbit", &["second", "argument"], env);
    assert_eq!(built.child.code, Some(0));
    let built_text = String::from_utf8_lossy(&built.stdout);
    assert!(built_text.contains("args=second argument"));
    assert!(built_text.contains("env=preserved"));
    assert!(built_text.contains(&format!("TREE pid={}", built.pid)));
    assert!(!harness.shim_state.join("installed.log").exists());

    proof_capture("A1", text(&refused));
    proof_capture("A2", text(&built));
    proof_capture("A4", text(&unclaimed));
}

#[test]
fn shell_init_restores_the_complete_door_prefix() {
    let harness = Harness::new();
    owned_fixture(&harness);
    let mut env = assembled(&harness, "repo");
    let expected = env["WT_PATH_PREFIX"].clone();
    env.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            harness.root.join("prepended").display(),
            env["PATH"]
        ),
    );
    env.insert(
        "WT_INIT".to_owned(),
        harness.json(&["shell-init", "zsh"])["data"]["script"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let output = capture_program(
        "/bin/bash",
        &[
            "--noprofile",
            "--norc",
            "-c",
            "eval \"$WT_INIT\"; printf '%s\\n' \"${PATH%%:*}\"; orbit before",
        ],
        env,
    );
    assert_eq!(output.child.code, Some(5));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected.split(':').next().unwrap()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PATH_NOT_SHADOWED"));
    assert!(stderr.contains("COMMAND_NOT_BUILT"));
    assert!(!stderr.contains("SHIM_INVOCATION_INVALID"));
}

#[test]
fn malformed_shim_invocations_refuse_without_guessing_a_tree() {
    let harness = Harness::new();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_wt"));

    let wrong_parent = harness.root.join("copied/orbit");
    wt_sys::fsx::replace_symlink(&wrong_parent, &binary).unwrap();
    let wrong = capture_program(wrong_parent.to_str().unwrap(), &[], BTreeMap::new());
    assert_eq!(wrong.child.code, Some(5));
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("SHIM_INVOCATION_INVALID"));

    let chain_target = harness.root.join("intermediate-wt");
    wt_sys::fsx::replace_symlink(&chain_target, &binary).unwrap();
    let chained = harness.root.join("tree/.wt/shims/orbit");
    wt_sys::fsx::replace_symlink(&chained, &chain_target).unwrap();
    let chain = capture_program(chained.to_str().unwrap(), &[], BTreeMap::new());
    assert_eq!(chain.child.code, Some(5));
    assert!(String::from_utf8_lossy(&chain.stderr).contains("must point directly"));

    let mut bare_request = CommandRequest::new("/bin/bash");
    bare_request.args = proc::os_args(&[
        "-c",
        "exec -a orbit \"$1\"",
        "bash",
        binary.to_str().unwrap(),
    ]);
    bare_request.clear_env = true;
    bare_request.env.insert(
        "PATH".to_owned(),
        "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
    );
    let bare = proc::capture(&bare_request, Duration::from_secs(10)).unwrap();
    assert_eq!(bare.child.code, Some(5));
    assert!(String::from_utf8_lossy(&bare.stderr).contains("bare argv[0]"));

    proof_capture(
        "A6",
        format!(
            "wrong parent:\n{}\nsymlink chain:\n{}\nbare argv[0]:\n{}",
            text(&wrong).trim_end(),
            text(&chain).trim_end(),
            text(&bare).trim_end()
        ),
    );
}

#[test]
fn one_interactive_shell_reaches_the_build_without_rehashing() {
    let harness = Harness::new();
    let repo = owned_fixture(&harness);
    let env = assembled(&harness, "repo");
    let mut transcripts = Vec::new();
    for (name, path, args) in [
        (
            "bash",
            Path::new("/bin/bash"),
            ["--noprofile", "--norc", "-i"].as_slice(),
        ),
        ("zsh", Path::new("/bin/zsh"), ["-f", "-i"].as_slice()),
    ] {
        if !path.is_file() {
            transcripts.push(format!("{name}: skipped (not installed)"));
            continue;
        }
        let built = repo.join("target/debug/orbit");
        if built.exists() {
            wt_sys::fsx::remove_path(&built).unwrap();
        }
        let mut request = CommandRequest::new(path);
        request.args = proc::os_args(args);
        request.clear_env = true;
        request.env = env.clone();
        let input = b"printf '__FIRST__\\n'\norbit before\nprintf '__BUILD__\\n'\nwt build repo\nprintf '__SECOND__\\n'\norbit after\nprintf '__DONE__\\n'\nexit\n";
        let output = proc::pty_capture(&request, input, Duration::from_secs(20)).unwrap();
        assert_eq!(
            output.child.code,
            Some(0),
            "{name} transcript: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let transcript = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        assert!(
            transcript.contains("COMMAND_NOT_BUILT"),
            "{name}: {transcript}"
        );
        assert!(transcript.contains("TREE pid="), "{name}: {transcript}");
        assert!(transcript.contains("args=after"), "{name}: {transcript}");
        assert!(!transcript.contains("hash -r"));
        transcripts.push(format!("{name}:\n{transcript}"));
    }
    proof_capture("A3", transcripts.join("\n"));
}

#[test]
fn shim_path_is_separate_from_bins_and_doctor_reports_damage_and_shadowing() {
    let harness = Harness::new();
    owned_fixture(&harness);
    let env = harness.json(&["env", "repo"]);
    let path = env["data"]["env"]["PATH"].as_str().unwrap();
    let registered_root = PathBuf::from(env["data"]["env"]["WT_ROOT"].as_str().unwrap());
    let shim_dir = registered_root.join(".wt/shims");
    assert_eq!(path.split(':').next(), Some(shim_dir.to_str().unwrap()));
    assert_eq!(
        env["data"]["env"]["WT_BIN"],
        registered_root
            .join("target/debug")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(env["data"]["bins"].as_array().unwrap().len(), 1);
    assert_ne!(
        env["data"]["bins"][0]["dir"],
        shim_dir.to_string_lossy().as_ref()
    );

    let shim = shim_dir.join("orbit");
    wt_sys::fsx::replace_symlink(&shim, &registered_root.join("missing-wt")).unwrap();
    let output = harness
        .wt()
        .env("WT_SHELL_SHADOWS", "orbit")
        .args(["doctor", "repo", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let doctor: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codes = doctor["data"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"SHIM_BROKEN"));
    assert!(codes.contains(&"SHIM_SHADOWED"));
    harness.json(&["env", "repo"]);
    assert!(wt_sys::fsx::is_executable_file(&shim).unwrap());

    proof_capture(
        "A5",
        format!(
            "PATH first: {}\nWT_BIN: {}\nbin inventory entries: {}",
            path.split(':').next().unwrap(),
            env["data"]["env"]["WT_BIN"].as_str().unwrap(),
            env["data"]["bins"].as_array().unwrap().len()
        ),
    );
    proof_capture("A6", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn environment_claim_overrides_restores_and_keeps_ports_out_of_children() {
    let harness = Harness::new();
    write(
        &harness.home.join("config.toml"),
        "[ports]\nbase=30000\nstride=16\n",
    );
    let repo = harness.repo(
        "repo",
        "ports=['http']\n[env]\nDATABASE_URL='tree-db'\nAPP_PORT=\"${ports.http}\"\n",
    );
    harness.register(&repo);
    let output = harness
        .wt()
        .env("DATABASE_URL", "production-db")
        .args(["env", "repo", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data"]["env"]["DATABASE_URL"], "tree-db");
    assert!(report["data"].get("kept").is_none());
    assert!(report["data"]["overrode"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("DATABASE_URL")));
    assert_eq!(
        report["data"]["activation"]["prior"]["DATABASE_URL"],
        "production-db"
    );
    assert_eq!(report["data"]["ports"][0]["port"], 30000);
    assert!(report["data"]["env"].get("WT_PORT_HTTP").is_none());
    assert!(report["data"]["env"].get("WT_PORT_BASE").is_none());
    assert_eq!(
        harness.json(&["status", "repo"])["data"]["ports"][0]["port"],
        30000
    );
    assert_eq!(
        harness.json(&["list"])["data"]["trees"][0]["ports"][0]["port"],
        30000
    );
    for verb in ["env", "exec", "shell", "run"] {
        let help = harness.wt().args([verb, "--help"]).output().unwrap();
        assert!(help.status.success());
        assert!(!String::from_utf8_lossy(&help.stdout).contains("force-env"));
    }

    let child = harness
        .wt()
        .env("DATABASE_URL", "production-db")
        .args([
            "exec",
            "repo",
            "--",
            "sh",
            "-c",
            "printf 'db=%s app=%s raw=%s base=%s\\n' \"$DATABASE_URL\" \"$APP_PORT\" \"${WT_PORT_HTTP-unset}\" \"${WT_PORT_BASE-unset}\"",
        ])
        .output()
        .unwrap();
    assert!(child.status.success());
    assert_eq!(
        String::from_utf8_lossy(&child.stdout),
        "db=tree-db app=30000 raw=unset base=unset\n"
    );

    let mut activated = report["data"]["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
        .collect::<BTreeMap<_, _>>();
    activated.insert(
        "WT_ACTIVATION".to_owned(),
        report["data"]["env"]["WT_ACTIVATION"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let mut deactivate = CommandRequest::new(env!("CARGO_BIN_EXE_wt"));
    deactivate.args = proc::os_args(&["env", "--deactivate", "--sh"]);
    deactivate.clear_env = true;
    deactivate.env = activated;
    let deactivated = proc::capture(&deactivate, Duration::from_secs(10)).unwrap();
    let inverse = String::from_utf8_lossy(&deactivated.stdout);
    assert!(inverse.contains("export DATABASE_URL='production-db'"));

    proof_capture("B1", String::from_utf8_lossy(&child.stdout));
    proof_capture("B2", inverse);
    proof_capture(
        "B3",
        format!(
            "ports report: {}={}\nchild: {}",
            report["data"]["ports"][0]["name"].as_str().unwrap(),
            report["data"]["ports"][0]["port"],
            String::from_utf8_lossy(&child.stdout).trim_end()
        ),
    );
}

#[test]
fn vars_and_functions_render_real_environment_and_files() {
    let harness = Harness::new();
    write(
        &harness.home.join("config.toml"),
        "[ports]\nbase=31000\nstride=16\n",
    );
    let repo = harness.repo(
        "repo",
        r#"ports=['http']
[vars]
leaf='value'
composed="${root()}/${leaf}/${ports.http}"
[env]
COMPOSED='${composed}'
ALL='${repo()}|${branch()}|${label()}|${name()}|${name_snake()}|${name_short()}|${target()}'
[files.generated]
marker=''
content='${composed}'
"#,
    );
    harness.register(&repo);
    let env = harness.json(&["env", "repo"]);
    let root = env["data"]["env"]["WT_ROOT"].as_str().unwrap();
    assert_eq!(
        env["data"]["env"]["COMPOSED"],
        format!("{root}/value/31000")
    );
    assert!(env["data"]["env"].get("leaf").is_none());
    assert!(env["data"]["env"].get("composed").is_none());
    let rendered = wt_sys::fsx::read_string(&repo.join("generated"))
        .unwrap()
        .unwrap();
    assert_eq!(rendered, format!("{root}/value/31000"));
    proof_capture(
        "C1",
        format!(
            "ALL={}\nCOMPOSED={}",
            env["data"]["env"]["ALL"], env["data"]["env"]["COMPOSED"]
        ),
    );
    proof_capture(
        "C2",
        format!("rendered={rendered}\nleaf exported=false\ncomposed exported=false"),
    );
}

#[test]
fn configuration_failures_are_captured_with_names_and_locations() {
    let cases = [
        ("cycle", "[vars]\na='${b}'\nb='${a}'", "VARS_CYCLE"),
        ("unknown", "[vars]\na='${missing}'", "VARS_UNKNOWN"),
        ("function", "[env]\nA='${mystery()}'", "CONFIG_INVALID"),
        (
            "port",
            "ports=['http']\n[env]\nA=\"${ports.missing}\"",
            "CONFIG_INVALID",
        ),
    ];
    let mut captured = Vec::new();
    for (name, source, code) in cases {
        let config = wt_core::config::parse(source, &format!("{name}.wt.toml")).unwrap();
        let error = wt_core::config::validate_resolved(&config, 16).unwrap_err();
        assert_eq!(error.code.0, code);
        assert!(error.message.contains(".wt.toml:"));
        captured.push(format!("{name}: {error}"));
    }
    proof_capture("C3", captured.join("\n"));
}

#[test]
fn legacy_template_spelling_is_rejected_at_its_source_location() {
    let source = "ports=['http']\n[env]\nAPP_PORT = \"$WT_PORT_HTTP\"\nROOT = '$WT_ROOT'\n";
    let error = wt_core::config::parse(source, "legacy.wt.toml").unwrap_err();
    assert_eq!(error.code.0, "CONFIG_INVALID");
    assert!(error.message.contains("legacy.wt.toml:3:13"));
    assert!(error.message.contains("$WT_PORT_HTTP"));
    assert!(error.message.contains("${ports.http}"));
}

#[test]
fn shim_fast_path_has_no_door_effects_and_is_well_below_the_door_budget() {
    let harness = Harness::new();
    let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    write(
        &harness.home.join("config.toml"),
        &format!("[ports]\nbase={port}\nstride=1\n[session]\nbackend='none'\n"),
    );
    let repo = owned_fixture(&harness);
    harness.json(&["build", "repo"]);
    let created = harness.json(&["new", "repo/work", "--no-sync"]);
    let work = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    let mut env = assembled(&harness, "repo");
    let mut refusal_env = assembled(&harness, "repo/work");
    write(&repo.join(".wt.toml"), "invalid = [\n");
    let budget_trace = harness.shim_state.join("fast-budget.jsonl");
    let spawn_trace = harness.shim_state.join("fast-spawn.jsonl");
    let lock_trace = harness.shim_state.join("fast-lock.jsonl");
    env.insert(
        "WT_BUDGET_TRACE".to_owned(),
        budget_trace.to_string_lossy().into_owned(),
    );
    env.insert(
        "WT_SPAWN_TRACE".to_owned(),
        spawn_trace.to_string_lossy().into_owned(),
    );
    env.insert(
        "WT_LOCK_TRACE_FILE".to_owned(),
        lock_trace.to_string_lossy().into_owned(),
    );
    let iterations = 30_u32;
    let started = Instant::now();
    for _ in 0..iterations {
        let output = capture_program("orbit", &[], env.clone());
        assert_eq!(output.child.code, Some(0));
    }
    let elapsed = started.elapsed();
    let mean_ms = elapsed.as_secs_f64() * 1000.0 / f64::from(iterations);
    assert!(mean_ms < 20.0, "mean fast-path cost was {mean_ms:.3} ms");

    wt_sys::fsx::remove_path(&work.join("target/debug/orbit")).unwrap();
    write(&work.join(".wt/build.status"), "running\n");
    refusal_env.insert(
        "WT_BUDGET_TRACE".to_owned(),
        budget_trace.to_string_lossy().into_owned(),
    );
    refusal_env.insert(
        "WT_SPAWN_TRACE".to_owned(),
        spawn_trace.to_string_lossy().into_owned(),
    );
    refusal_env.insert(
        "WT_LOCK_TRACE_FILE".to_owned(),
        lock_trace.to_string_lossy().into_owned(),
    );
    write(&work.join(".wt.toml"), "invalid = [\n");
    let refusal_started = Instant::now();
    for _ in 0..iterations {
        let output = capture_program("orbit", &[], refusal_env.clone());
        assert_eq!(output.child.code, Some(5));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("COMMAND_NOT_BUILT"));
        assert!(stderr.contains("build is in progress"));
        assert!(stderr.contains("wt build repo/work"));
    }
    let refusal_elapsed = refusal_started.elapsed();
    let refusal_mean_ms = refusal_elapsed.as_secs_f64() * 1000.0 / f64::from(iterations);
    assert!(
        refusal_mean_ms < 20.0,
        "mean refusal-path cost was {refusal_mean_ms:.3} ms"
    );
    assert!(
        !budget_trace.exists(),
        "fast path performed a traced door read"
    );
    assert!(
        !spawn_trace.exists(),
        "fast path spawned instead of execing"
    );
    assert!(!lock_trace.exists(), "fast path acquired a traced lock");
    proof_capture(
        "H3",
        format!(
            "success iterations={iterations}\nsuccess elapsed_ms={:.3}\nsuccess mean_ms={mean_ms:.3}\nrefusal iterations={iterations}\nrefusal elapsed_ms={:.3}\nrefusal mean_ms={refusal_mean_ms:.3}\nrecorded build and status read=true\ninvalid_config=ignored\ndoor_effect_trace=absent\nlock_trace=absent\nspawn_trace=absent",
            elapsed.as_secs_f64() * 1000.0,
            refusal_elapsed.as_secs_f64() * 1000.0
        ),
    );
}
