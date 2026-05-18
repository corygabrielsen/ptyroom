use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

const USER_FACING_CRATES: [&str; 4] = ["ptyrecord", "ptyrender", "ptyroom", "ptytrace"];

#[test]
fn workspace_contains_only_user_facing_crates() {
    let root = workspace_root().join("crates");
    let actual = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            path.is_dir()
                .then(|| entry.file_name().into_string().unwrap())
        })
        .collect::<BTreeSet<_>>();
    let expected = USER_FACING_CRATES
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    assert_eq!(actual, expected);
}

#[test]
fn member_manifests_do_not_define_hidden_bin_targets() {
    for package in USER_FACING_CRATES {
        let manifest = member_manifest(package);

        assert!(
            !manifest
                .lines()
                .any(|line| line.split('#').next().unwrap_or("").trim() == "[[bin]]"),
            "{package} should rely on Cargo's visible binary auto-discovery"
        );
    }
}

#[test]
fn root_workspace_members_match_command_algebra() {
    let metadata = workspace_metadata();
    let expected = USER_FACING_CRATES
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    assert_eq!(workspace_member_names(&metadata), expected);
}

#[test]
fn member_dependencies_follow_command_algebra() {
    // Dep algebra (DAG, strict partial order):
    //
    //   ptytrace   <- the trace primitive; depends on nothing
    //   ptyrender  <- adds rendering; depends on ptytrace
    //   ptyrecord  <- adds bundling; depends on ptytrace + ptyrender
    //   ptyroom    <- live multi-client share + record umbrella;
    //                 depends on all three (it owns the user-facing
    //                 `host` binary that produces .ptyrecord + .mp4
    //                 alongside the trace, mirroring ptyrecord's
    //                 solo-session output set for symmetry)
    //
    // The rule was previously stricter — ptyroom depended only on
    // ptytrace, with rendering deferred to a manual `ptyrender X`
    // step. That made `ptyroom host` produce a `.ptytrace` that
    // users couldn't play without running a second command and
    // having the other binaries on $PATH. Library composability
    // (single `cargo install ptyroom` ships the whole thing,
    // version-locked at one workspace commit) outweighed the
    // separation. The dep graph remains a DAG; ptyroom is just at
    // the top of the poset now.
    let metadata = workspace_metadata();

    assert_dependencies(
        &metadata,
        "ptytrace",
        &[],
        &["ptyrender", "ptyrecord", "ptyroom"],
    );
    assert_dependencies(
        &metadata,
        "ptyrender",
        &["ptytrace"],
        &["ptyrecord", "ptyroom"],
    );
    assert_dependencies(
        &metadata,
        "ptyrecord",
        &["ptytrace", "ptyrender"],
        &["ptyroom"],
    );
    assert_dependencies(
        &metadata,
        "ptyroom",
        &["ptytrace", "ptyrender", "ptyrecord"],
        &[],
    );
}

#[test]
fn render_pipeline_modules_are_owned_by_ptyrender() {
    let root = workspace_root().join("crates");
    let ptytrace_src = root.join("ptytrace").join("src");
    let ptyrender_src = root.join("ptyrender").join("src");

    for path in [
        "encode.rs",
        "frame.rs",
        "frame_replay",
        "inspect.rs",
        "paint.rs",
        "render.rs",
        "render_cli.rs",
        "verify.rs",
        "witness.rs",
    ] {
        assert!(
            !ptytrace_src.join(path).exists(),
            "render-owned module {path} should not live in ptytrace"
        );
        assert!(
            ptyrender_src.join(path).exists(),
            "ptyrender should own render module {path}"
        );
    }

    let ptytrace_lib = std::fs::read_to_string(ptytrace_src.join("lib.rs")).unwrap();
    assert!(!ptytrace_lib.contains("pub use render"));
    assert!(!ptytrace_lib.contains("pub mod witness"));
}

#[test]
fn member_crates_have_package_specific_readmes() {
    let metadata = workspace_metadata();
    for package in USER_FACING_CRATES {
        let metadata_readme = package_metadata(&metadata, package)
            .get("readme")
            .and_then(Value::as_str);
        assert_eq!(metadata_readme, Some("README.md"));
        assert!(
            workspace_root()
                .join("crates")
                .join(package)
                .join("README.md")
                .exists(),
            "{package} README.md is missing"
        );
    }
}

#[test]
fn cli_surfaces_follow_command_algebra() {
    let ptytrace_help = cargo_stdout(&["run", "--quiet", "-p", "ptytrace", "--", "--help"]);
    for command in ["capture", "run", "attest-file", "stitch", "check"] {
        assert!(
            ptytrace_help.contains(&format!("\n  {command}")),
            "ptytrace help should expose {command}"
        );
    }
    for command in ["render", "verify", "debug"] {
        assert!(
            !ptytrace_help.contains(&format!("\n  {command}")),
            "ptytrace help should not expose {command}"
        );
    }

    let ptyrender_help = cargo_stdout(&["run", "--quiet", "-p", "ptyrender", "--", "--help"]);
    assert!(ptyrender_help.contains("ptyrender <trace.ptytrace> <out.gif|out.mp4>"));
    assert!(ptyrender_help.contains("ptyrender verify --witness <witness.json>"));

    let ptyrender_verify_help = cargo_stdout(&[
        "run",
        "--quiet",
        "-p",
        "ptyrender",
        "--",
        "verify",
        "--help",
    ]);
    assert!(ptyrender_verify_help.contains("Usage: ptyrender verify"));
}

fn assert_dependencies(metadata: &Value, package: &str, required: &[&str], forbidden: &[&str]) {
    let deps = direct_dependency_names(metadata, package);
    for dep in required {
        assert!(deps.contains(*dep), "{package} must depend on {dep}");
    }
    for dep in forbidden {
        assert!(!deps.contains(*dep), "{package} must not depend on {dep}");
    }
}

fn direct_dependency_names(metadata: &Value, package: &str) -> BTreeSet<String> {
    package_metadata(metadata, package)
        .get("dependencies")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|dep| dep.get("name").and_then(Value::as_str).unwrap().to_owned())
        .collect()
}

fn workspace_member_names(metadata: &Value) -> BTreeSet<String> {
    let workspace_member_ids = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect::<BTreeSet<_>>();

    metadata
        .get("packages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|package| {
            let id = package.get("id").and_then(Value::as_str).unwrap();
            workspace_member_ids.contains(id)
        })
        .map(|package| {
            package
                .get("name")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn package_metadata<'a>(metadata: &'a Value, package: &str) -> &'a Value {
    metadata
        .get("packages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|value| value.get("name").and_then(Value::as_str) == Some(package))
        .unwrap_or_else(|| panic!("missing package metadata for {package}"))
}

fn workspace_metadata() -> Value {
    let output = cargo_command()
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn cargo_stdout(args: &[&str]) -> String {
    let output = cargo_command()
        .args(args)
        .current_dir(workspace_root())
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn cargo_command() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn member_manifest(package: &str) -> String {
    std::fs::read_to_string(
        workspace_root()
            .join("crates")
            .join(package)
            .join("Cargo.toml"),
    )
    .unwrap()
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ptyroom crate lives under <workspace>/crates/ptyroom")
}
