mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{git, proof_capture, write, write_executable, Harness};
use predicates::prelude::*;
use wt_sys::proc::{self, CommandRequest};

const ORBIT_CONFIG: &str = include_str!("../spec/acceptance/orbit.wt.toml");
const ORBITAPP_CONFIG: &str = include_str!("../spec/acceptance/orbitapp.wt.toml");
const ORBITCLOUD_CONFIG: &str = include_str!("../spec/acceptance/orbitcloud.wt.toml");

const ORBIT_STUB: &str = r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$WT_SHIM_STATE/orbit-tree.log"
state_dir=$(dirname "$ORBIT_CONFIG")/state
state="$state_dir/stub.state"
case "${1-} ${2-}" in
  "server start") mkdir -p "$state_dir"; : > "$state" ;;
  "server stop") rm -f "$state" ;;
  "list ") test -f "$state" ;;
  *) exit 64 ;;
esac
"#;

const DOCKER_STUB: &str = r#"#!/bin/sh
set -eu
printf 'docker' >> "$WT_SHIM_STATE/docker.log"
for arg in "$@"; do printf '\t%s' "$arg" >> "$WT_SHIM_STATE/docker.log"; done
printf '\n' >> "$WT_SHIM_STATE/docker.log"
case "${1-} ${2-}" in
  "info ") test -f "$WT_SHIM_STATE/docker-up" ;;
  "container inspect") test -f "$WT_SHIM_STATE/docker-container" ;;
  "ps -aq") test -f "$WT_SHIM_STATE/docker-container" && printf 'container-id\n' ;;
  "rm -f") rm -f "$WT_SHIM_STATE/docker-container" ;;
  "volume ls") test -f "$WT_SHIM_STATE/docker-volume" && printf 'volume-id\n' ;;
  "volume rm") rm -f "$WT_SHIM_STATE/docker-volume" ;;
  "network ls") test -f "$WT_SHIM_STATE/docker-network" && printf 'network-id\n' ;;
  "network rm") rm -f "$WT_SHIM_STATE/docker-network" ;;
  *) exit 64 ;;
esac
"#;

fn install_probe_agent(harness: &Harness) {
    write(
        &harness.home.join("config.toml"),
        "[session]\nbackend='tmux'\nagent='probe'\n[agents.probe]\nstart=['true']\nresume=['true']\n",
    );
}

fn commit_fixture(repo: &Path) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "acceptance fixture"]);
    git(repo, &["push", "-q", "origin", "main"]);
}

fn make_orbit_fixture(harness: &Harness) -> PathBuf {
    let repo = harness.repo("orbit", ORBIT_CONFIG);
    write(
        &repo.join("Cargo.toml"),
        "[workspace]\nmembers=['orbit-cli']\ndefault-members=['orbit-cli']\n",
    );
    write(
        &repo.join("orbit-cli/Cargo.toml"),
        "[package]\nname='orbit-cli'\nversion='0.0.0'\nedition='2021'\n",
    );
    write(&repo.join("orbit-cli/src/main.rs"), "fn main() {}\n");
    write_executable(&repo.join("fixture/orbit"), ORBIT_STUB);
    commit_fixture(&repo);

    write_executable(&repo.join("target/debug/orbit"), ORBIT_STUB);
    write_executable(
        &harness.shims.join("cargo"),
        r#"#!/bin/sh
set -eu
printf 'cargo' >> "$WT_SHIM_STATE/cargo.log"
for arg in "$@"; do printf '\t%s' "$arg" >> "$WT_SHIM_STATE/cargo.log"; done
printf '\n' >> "$WT_SHIM_STATE/cargo.log"
mkdir -p target/debug
cp fixture/orbit target/debug/orbit
chmod +x target/debug/orbit
"#,
    );
    write_executable(
        &harness.shims.join("orbit"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$WT_SHIM_STATE/orbit-installed.log\"\nexit 99\n",
    );
    repo
}

fn make_orbitapp_fixture(harness: &Harness) -> PathBuf {
    let sibling = harness.repos.join("orbit/crates/orbit");
    wt_sys::fsx::create_private_dir(&sibling).unwrap();
    write(&sibling.join("Cargo.toml"), "[package]\nname='orbit'\n");

    let repo = harness.repo("orbitapp", ORBITAPP_CONFIG);
    write(
        &repo.join("package.json"),
        r#"{"name":"orbitapp","scripts":{"ios":"true"}}"#,
    );
    write(&repo.join("package-lock.json"), "{}\n");
    commit_fixture(&repo);
    write_executable(
        &harness.shims.join("npm"),
        r#"#!/bin/sh
printf 'npm' >> "$WT_SHIM_STATE/npm.log"
for arg in "$@"; do printf '\t%s' "$arg" >> "$WT_SHIM_STATE/npm.log"; done
printf '\n' >> "$WT_SHIM_STATE/npm.log"
"#,
    );
    repo
}

fn make_orbitcloud_fixture(harness: &Harness) -> PathBuf {
    let repo = harness.repo("orbitcloud", ORBITCLOUD_CONFIG);
    write(
        &repo.join("orbitcloud.sln"),
        "Microsoft Visual Studio Solution File\n",
    );
    for directory in ["frontend", "website"] {
        write(
            &repo.join(directory).join("package.json"),
            &format!(r#"{{"name":"{directory}"}}"#),
        );
        write(&repo.join(directory).join("package-lock.json"), "{}\n");
    }
    commit_fixture(&repo);

    write(&repo.join(".mcp.json"), "{\"fixture\":true}\n");
    write(
        &repo.join(".claude/settings.local.json"),
        "{\"permissions\":{}}\n",
    );
    write(
        &repo.join(".git/info/exclude"),
        ".mcp.json\n.claude/settings.local.json\n",
    );
    write_executable(
        &harness.shims.join("dotnet"),
        r#"#!/bin/sh
printf 'dotnet' >> "$WT_SHIM_STATE/dotnet.log"
for arg in "$@"; do printf '\t%s' "$arg" >> "$WT_SHIM_STATE/dotnet.log"; done
printf '\n' >> "$WT_SHIM_STATE/dotnet.log"
"#,
    );
    write_executable(
        &harness.shims.join("npm"),
        r#"#!/bin/sh
printf 'npm' >> "$WT_SHIM_STATE/npm.log"
for arg in "$@"; do printf '\t%s' "$arg" >> "$WT_SHIM_STATE/npm.log"; done
printf '\n' >> "$WT_SHIM_STATE/npm.log"
"#,
    );
    write_executable(&harness.shims.join("docker"), DOCKER_STUB);
    repo
}

fn resource_reason(harness: &Harness, target: &str) -> Option<String> {
    let target = wt_core::model::Target::parse(target).unwrap();
    let state = wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(
        &harness.home.join(wt_core::model::tree_state_path(&target)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    state
        .resources
        .values()
        .next()
        .and_then(|record| record.reason.clone())
}

fn sha256_prefix(path: &Path, length: usize) -> String {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    let mut request = CommandRequest::new("sh");
    request.args = proc::os_args(&[
        "-c",
        "printf %s \"$1\" | shasum -a 256 | cut -c1-$2",
        "sh",
        &lower,
        &length.to_string(),
    ]);
    let output = proc::capture(&request, Duration::from_secs(10)).unwrap();
    assert!(output.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn orbit_acceptance_walk_covers_rendered_config_daemon_and_safe_missing_tree_teardown() {
    let h = Harness::new();
    install_probe_agent(&h);
    let repo = make_orbit_fixture(&h);

    let registered = h.register(&repo);
    assert_eq!(registered["data"]["tree"]["target"], "orbit");
    let canonical_config = repo.join(".wt/orbit/config.yaml");
    let canonical = std::fs::read_to_string(&canonical_config).unwrap();
    let canonical_short = wt_core::model::name_short("orbit", "canonical");
    assert!(canonical.contains(&format!("host_name: {canonical_short}")));
    let canonical_root = registered["data"]["tree"]["path"].as_str().unwrap();
    assert!(canonical.contains(&format!(
        "socket_path: {canonical_root}/.wt/orbit/orbit.sock"
    )));
    let canonical_env = h.json(&["env", "orbit"]);
    proof_capture(
        "C4",
        format!(
            "orbit effective environment:\nORBIT_CONFIG={}\nORBIT_LOG={}\nORBIT_INVARIANT_FATAL={}\norbit rendered file:\n{}",
            canonical_env["data"]["env"]["ORBIT_CONFIG"],
            canonical_env["data"]["env"]["ORBIT_LOG"],
            canonical_env["data"]["env"]["ORBIT_INVARIANT_FATAL"],
            canonical.trim_end()
        ),
    );
    h.json(&["run", "daemon", "orbit"]);
    assert!(repo.join(".wt/orbit/state/stub.state").exists());

    let created = h.json(&["new", "orbit/feature", "--no-sync"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    let run = h.json(&["run", "daemon", "orbit/feature"]);
    assert_eq!(run["data"]["steps"][0]["id"], "build");
    assert_eq!(run["data"]["steps"][1]["id"], "daemon");
    let rendered = std::fs::read_to_string(tree.join(".wt/orbit/config.yaml")).unwrap();
    assert!(rendered.contains(&format!(
        "host_name: {}",
        wt_core::model::name_short("orbit", "feature")
    )));
    assert!(rendered.contains(&format!(
        "state_path: {}/.wt/orbit/state/state.yaml",
        tree.display()
    )));
    assert!(rendered.contains(&format!("data_dir: {}/.wt/orbit/data", tree.display())));
    assert!(tree.join(".wt/orbit/state/stub.state").exists());
    assert!(std::fs::read_to_string(h.shim_state.join("cargo.log"))
        .unwrap()
        .contains("cargo\tbuild\t--workspace"));

    h.json(&["open", "orbit/feature", "--no-attach"]);
    let removed = h.json(&["remove", "orbit/feature", "--yes", "--force"]);
    assert_eq!(removed["data"]["removed"], true);
    assert_eq!(removed["data"]["session_closed"], true);
    assert!(!tree.exists());
    assert!(repo.join(".wt/orbit/state/stub.state").exists());
    let orbit_log = std::fs::read_to_string(h.shim_state.join("orbit-tree.log")).unwrap();
    for command in ["server start", "list", "server stop"] {
        assert!(orbit_log.lines().any(|line| line == command));
    }

    let lost = h.json(&["new", "orbit/lost", "--no-sync"]);
    let lost_path = PathBuf::from(lost["data"]["tree"]["path"].as_str().unwrap());
    h.json(&["run", "daemon", "orbit/lost"]);
    wt_sys::fsx::remove_path(&lost_path).unwrap();
    h.wt()
        .args(["prune", "orbit", "--yes", "--json"])
        .assert()
        .code(6)
        .stdout(predicate::str::contains("DESTROY_FAILED"));
    assert_eq!(
        resource_reason(&h, "orbit/lost").as_deref(),
        Some("exe_missing")
    );
    assert!(!h.shim_state.join("orbit-installed.log").exists());
}

#[test]
fn orbitapp_acceptance_walk_covers_port_alias_sibling_link_and_path_occupied() {
    let h = Harness::new();
    install_probe_agent(&h);
    let repo = make_orbitapp_fixture(&h);

    h.register(&repo);
    let created = h.json(&["new", "orbitapp/feature"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    let env = h.json(&["env", "orbitapp/feature"]);
    assert_eq!(env["data"]["env"]["RCT_METRO_PORT"], "20016");
    assert_eq!(env["data"]["ports"][0]["name"], "metro");
    assert_eq!(env["data"]["ports"][0]["port"], 20016);
    assert!(env["data"]["env"].get("WT_PORT_METRO").is_none());
    proof_capture(
        "C4",
        format!(
            "orbitapp effective environment and ports:\nRCT_METRO_PORT={}\nmetro={}",
            env["data"]["env"]["RCT_METRO_PORT"], env["data"]["ports"][0]["port"]
        ),
    );

    let run = h.json(&["run", "ios", "orbitapp/feature"]);
    assert_eq!(run["data"]["steps"][0]["id"], "orbit-src");
    assert_eq!(run["data"]["steps"][1]["id"], "ios");
    let link = tree.parent().unwrap().join("orbit");
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(repo.parent().unwrap().join("orbit")).unwrap()
    );
    assert!(std::fs::read_to_string(h.shim_state.join("npm.log"))
        .unwrap()
        .contains("npm\trun\tios"));

    h.json(&["open", "orbitapp/feature", "--no-attach"]);
    assert_eq!(
        h.json(&["remove", "orbitapp/feature", "--yes"])["data"]["removed"],
        true
    );
    assert!(!tree.exists());
    h.wt()
        .args(["new", "orbitapp/orbit", "--no-sync", "--json"])
        .assert()
        .code(5)
        .stdout(predicate::str::contains("PATH_OCCUPIED"));
}

#[test]
fn orbitcloud_acceptance_walk_covers_composition_copy_no_run_resource_and_probe_barrier() {
    let h = Harness::new();
    install_probe_agent(&h);
    let repo = make_orbitcloud_fixture(&h);
    write(&h.shim_state.join("docker-up"), "up\n");

    h.register(&repo);
    let created = h.json(&["new", "orbitcloud/feature"]);
    let tree = PathBuf::from(created["data"]["tree"]["path"].as_str().unwrap());
    assert_eq!(
        std::fs::read(tree.join(".mcp.json")).unwrap(),
        b"{\"fixture\":true}\n"
    );
    assert_eq!(
        std::fs::read(tree.join(".claude/settings.local.json")).unwrap(),
        b"{\"permissions\":{}}\n"
    );
    let status = git_status(&tree);
    assert!(
        status.is_empty(),
        "copied files must stay excluded: {status}"
    );
    let sync_log = std::fs::read_to_string(h.shim_state.join("dotnet.log")).unwrap();
    assert!(sync_log.contains("dotnet\trestore\torbitcloud.sln"));
    let npm_log = std::fs::read_to_string(h.shim_state.join("npm.log")).unwrap();
    assert!(npm_log.contains("npm\t--prefix\tfrontend\tci"));
    assert!(npm_log.contains("npm\t--prefix\twebsite\tci"));

    let env = h.json(&["env", "orbitcloud/feature"]);
    let ports = env["data"]["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|port| {
            (
                port["name"].as_str().unwrap(),
                port["port"].as_u64().unwrap().to_string(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for (alias, port) in [
        ("Local__Ports__Postgres", "postgres"),
        ("Local__Ports__Server", "server"),
        ("Local__Ports__ServerHttps", "server_https"),
        ("Local__Ports__Frontend", "frontend"),
        ("Local__Ports__Website", "website"),
        ("Local__Ports__Dashboard", "dashboard"),
        ("Local__Ports__DashboardOtlp", "otlp"),
    ] {
        assert_eq!(env["data"]["env"][alias], ports[port]);
    }
    assert!(env["data"]["env"].get("WT_PORT_BASE").is_none());
    proof_capture(
        "C4",
        format!(
            "orbitcloud effective ports and environment:\nports={}\nPostgres={}\nServer={}\nServerHttps={}\nFrontend={}\nWebsite={}\nDashboard={}\nDashboardOtlp={}\nrendered copies:\n.mcp.json={}\n.claude/settings.local.json={}",
            serde_json::to_string(&env["data"]["ports"]).unwrap(),
            env["data"]["env"]["Local__Ports__Postgres"],
            env["data"]["env"]["Local__Ports__Server"],
            env["data"]["env"]["Local__Ports__ServerHttps"],
            env["data"]["env"]["Local__Ports__Frontend"],
            env["data"]["env"]["Local__Ports__Website"],
            env["data"]["env"]["Local__Ports__Dashboard"],
            env["data"]["env"]["Local__Ports__DashboardOtlp"],
            String::from_utf8_lossy(&std::fs::read(tree.join(".mcp.json")).unwrap()).trim_end(),
            String::from_utf8_lossy(
                &std::fs::read(tree.join(".claude/settings.local.json")).unwrap()
            )
            .trim_end()
        ),
    );
    let pgdata = h.json(&["run", "pgdata", "orbitcloud/feature"]);
    assert_eq!(pgdata["data"]["child"], serde_json::Value::Null);
    assert!(pgdata["notices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["message"] == "declared; created by the application"));

    for name in ["docker-container", "docker-volume", "docker-network"] {
        write(&h.shim_state.join(name), "present\n");
    }
    h.json(&["run", "pgdata", "orbitcloud/feature"]);
    h.json(&["open", "orbitcloud/feature", "--no-attach"]);
    assert_eq!(
        h.json(&["remove", "orbitcloud/feature", "--yes"])["data"]["removed"],
        true
    );
    assert!(!tree.exists());
    for name in ["docker-container", "docker-volume", "docker-network"] {
        assert!(!h.shim_state.join(name).exists());
    }
    let docker_log = std::fs::read_to_string(h.shim_state.join("docker.log")).unwrap();
    let apphost = tree.join("orbitcloud.AppHost/orbitcloud.AppHost.csproj");
    let hash8 = sha256_prefix(&apphost, 8);
    let hash10 = sha256_prefix(&apphost, 10);
    assert!(docker_log.contains(&format!("postgres-{hash8}")));
    assert!(docker_log.contains(&format!("orbitcloud.apphost-{hash10}-")));
    assert!(docker_log.contains(&format!("aspire-persistent-network-{hash8}-")));

    let down = h.json(&["new", "orbitcloud/docker-down", "--no-sync"]);
    let down_path = PathBuf::from(down["data"]["tree"]["path"].as_str().unwrap());
    wt_sys::fsx::remove_path(&h.shim_state.join("docker-up")).unwrap();
    h.wt()
        .args(["remove", "orbitcloud/docker-down", "--yes", "--json"])
        .assert()
        .code(6)
        .stdout(predicate::str::contains("DESTROY_FAILED"));
    assert!(down_path.exists());
    assert_eq!(
        resource_reason(&h, "orbitcloud/docker-down").as_deref(),
        Some("probe_failed")
    );
    assert_eq!(
        h.json(&[
            "remove",
            "orbitcloud/docker-down",
            "--yes",
            "--keep-orphans"
        ])["data"]["removed"],
        true
    );
    assert_eq!(
        h.json(&["status", "orbitcloud/docker-down"])["data"]["phase"],
        "missing"
    );
    write(&h.shim_state.join("docker-up"), "up\n");
    assert_eq!(
        h.json(&["prune", "--records", "orbitcloud/docker-down", "--yes"])["data"]["items"][0]
            ["result"]["remaining"],
        0
    );
}

#[test]
fn version_matches_workspace_package_metadata() {
    let h = Harness::new();
    h.wt().arg("--version").assert().success().stdout(concat!(
        "wt ",
        env!("CARGO_PKG_VERSION"),
        "\n"
    ));
}

fn git_status(repo: &Path) -> String {
    let mut request = CommandRequest::new("git");
    request.cwd = Some(repo.to_path_buf());
    request.args = proc::os_args(&["status", "--porcelain"]);
    let output = proc::capture(&request, Duration::from_secs(10)).unwrap();
    assert!(output.success());
    String::from_utf8(output.stdout).unwrap()
}
