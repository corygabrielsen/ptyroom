use std::collections::BTreeSet;
use std::path::Path;

const USER_FACING_BINS: [&str; 4] = ["ptyrecord", "ptyrender", "ptyroom", "ptytrace"];

#[test]
fn src_bin_contains_only_user_facing_commands() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin");
    let actual = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            let is_bin = path.is_file() || path.join("main.rs").is_file();
            is_bin.then(|| entry.file_name().into_string().unwrap())
        })
        .collect::<BTreeSet<_>>();
    let expected = USER_FACING_BINS
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    assert_eq!(actual, expected);
}

#[test]
fn cargo_manifest_does_not_define_hidden_bin_targets() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();

    assert!(
        !manifest.lines().any(|line| line.trim() == "[[bin]]"),
        "bin targets should be auto-discovered from src/bin so the visible command surface has one source"
    );
}
