// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tests of the verification plan (`cargo xtask verify`).

use super::*;
use crate::testutil::Tree;

fn joined(steps: &[Vec<String>]) -> Vec<String> {
    steps.iter().map(|a| a.join(" ")).collect()
}

// `verify sta` runs the unit tests and the suites of the command line, and the tests of
// `rusteda-compute`, on the build without GPU runtimes; `--gpu` builds the command line with every
// GPU runtime, adds its ignored tests and runs the device self-test of `rusteda-compute`.
// `verify compute`, `verify sdc` and `verify all` are supported; other products are unknown.
#[test]
fn sta_plan() {
    let root = crate::root();
    let cli_dir = root.join("crates/apps/ruststa-cli");
    let suites = CLI_SUITES
        .into_iter()
        .filter(|t| target_exists(&root, &cli_dir, t))
        .map(|t| format!("--test {t}"))
        .collect::<Vec<_>>()
        .join(" ");
    let p = joined(&plan("sta", Options::default()).unwrap_or_else(|e| panic!("{e}")));
    assert_eq!(
        p.first().map(String::as_str),
        Some(
            format!("test --release -p ruststa-cli --no-default-features --lib {suites}").as_str()
        )
    );
    assert_eq!(
        p.last().map(String::as_str),
        Some("test --release -p rusteda-compute --lib")
    );
    let g = joined(&plan("sta", Options { gpu: true }).unwrap_or_else(|e| panic!("{e}")));
    assert_eq!(
        g.first().map(String::as_str),
        Some(
            format!(
                "test --release -p ruststa-cli --features cuda --lib {suites} -- --include-ignored"
            )
            .as_str()
        )
    );
    assert_eq!(
        g.last().map(String::as_str),
        Some("test --release -p rusteda-compute --features cuda --lib -- --ignored")
    );
    assert!(plan("compute", Options::default()).is_ok());
    assert!(plan("sdc", Options::default()).is_ok());
    assert!(plan("all", Options::default()).is_ok());
    assert!(plan("check", Options::default()).is_err());
    assert!(plan("", Options::default()).is_err());
}

// Suite files under `verification/suites/<domain>/*.toml` are loaded in deterministic sorted path
// order, and duplicate suite names or malformed entries are rejected.
#[test]
fn loads_suites_in_sorted_order() {
    let t = Tree::new("verify-suites");
    t.file(
        "verification/suites/sta/b_second.toml",
        concat!(
            "[[suite]]\nname = \"suite-b\"\nrequirement = [\"STA",
            "-REQ-0100\"]\ntest = \"crates/engines/ruststa/tests/b.rs\"\n"
        ),
    )
    .file(
        "verification/suites/sta/a_first.toml",
        concat!(
            "[[suite]]\nname = \"suite-a\"\nrequirement = [\"STA",
            "-REQ-0011\"]\ntest = \"crates/engines/ruststa/tests/a.rs\"\ndescription = \"first\"\n"
        ),
    );
    let loaded = load_domain(t.root(), "sta").unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<&str> = loaded.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["suite-a", "suite-b"]);

    // Duplicate suite names across files fail.
    t.file(
        "verification/suites/sta/c_dup.toml",
        concat!(
            "[[suite]]\nname = \"suite-a\"\nrequirement = [\"STA",
            "-REQ-0011\"]\ntest = \"crates/engines/ruststa/tests/c.rs\"\n"
        ),
    );
    assert!(load_domain(t.root(), "sta").is_err_and(|e| e.contains("duplicate suite 'suite-a'")));
}

/// The value of `key = …` in the table `[section]` of the manifest text, if any.
fn manifest_value(text: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut inside = false;
    text.lines().find_map(|l| {
        let l = l.trim();
        if l.starts_with('[') {
            inside = l == header;
            return None;
        }
        let (k, v) = l.split_once('=')?;
        (inside && k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

// On the repository itself, the plan names only packages, features and targets that exist: the
// command-line crate with its GPU features, its library and its integration tests, every suite of
// `verification/suites/sta/*.toml` among them, and `rusteda-compute` with its features and an
// ignored test in its library.
#[test]
fn sta_plan_names_existing_targets() {
    let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return;
    };
    let cli = root.join("crates/apps/ruststa-cli");
    let common = crate::foundation::root(root)
        .unwrap()
        .unwrap_or_else(|| root.to_path_buf());
    let gpu = common.join("crates/core/rusteda-compute");
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let (cli_toml, gpu_toml) = (read(&cli.join("Cargo.toml")), read(&gpu.join("Cargo.toml")));
    assert_eq!(
        manifest_value(&cli_toml, "package", "name").as_deref(),
        Some("ruststa-cli")
    );
    assert_eq!(
        manifest_value(&gpu_toml, "package", "name").as_deref(),
        Some("rusteda-compute")
    );
    for f in GPU_FEATURES.split(',') {
        assert!(
            manifest_value(&cli_toml, "features", f).is_some(),
            "ruststa-cli feature {f}"
        );
        assert!(
            manifest_value(&gpu_toml, "features", f).is_some(),
            "rusteda-compute feature {f}"
        );
    }
    assert_eq!(
        manifest_value(&cli_toml, "lib", "path").as_deref(),
        Some("src/lib.rs")
    );
    assert!(
        cli.join("src/lib.rs").is_file(),
        "crates/apps/ruststa-cli/src/lib.rs"
    );
    for t in CLI_SUITES {
        let tests = cli.join("tests");
        assert!(
            tests.join(format!("{t}.rs")).is_file() || tests.join(t).join("main.rs").is_file(),
            "crates/apps/ruststa-cli/tests/{t}.rs"
        );
    }
    let sta_suites = load_domain(root, "sta").unwrap_or_else(|e| panic!("{e}"));
    assert!(!sta_suites.is_empty());
    for s in &sta_suites {
        assert!(
            source_path(root, Some(&common), &s.test).is_file(),
            "{}: missing test file {}",
            s.file,
            s.test
        );
        if let Some(target) = s
            .test
            .strip_prefix("crates/apps/ruststa-cli/tests/")
            .and_then(|t| t.strip_suffix(".rs"))
        {
            assert!(
                CLI_SUITES.contains(&target),
                "suite {} is not run by `verify sta`",
                s.test
            );
        }
    }
    let compute_suites = load_domain(root, "compute").unwrap_or_else(|e| panic!("{e}"));
    assert!(!compute_suites.is_empty());
    for s in &compute_suites {
        assert!(
            source_path(root, Some(&common), &s.test).is_file(),
            "{}: missing test file {}",
            s.file,
            s.test
        );
    }
    assert!(
        read(&gpu.join("src/lib.rs")).contains("#[ignore]"),
        "crates/core/rusteda-compute/src/lib.rs has no ignored test"
    );
}

#[test]
fn relocated_shared_test_paths_remain_resolvable() {
    let root = Tree::new("local-suite-paths");
    let common = Tree::new("canonical-suite-paths");
    let relative = "crates/formats/rusteda-formats/tests/constraints.rs";
    common.file(relative, "canonical test");
    assert!(source_path(root.root(), Some(common.root()), relative).is_file());
    assert!(!source_path(root.root(), None, relative).is_file());
    let tool = "crates/engines/ruststa/tests/own.rs";
    common.file(tool, "must not be borrowed");
    assert!(!source_path(root.root(), Some(common.root()), tool).is_file());
}
