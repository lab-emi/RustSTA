// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `cargo xtask verify [sta|sdc|compute|all] [--gpu]`: loads all
//! `verification/suites/<domain>/*.toml` files in deterministic sorted path order and runs every
//! registered verification suite.
//!
//! Without `--gpu`, `verify sta` runs on the build without GPU runtimes (`--no-default-features`),
//! which a machine without a GPU can test in full, the errors for a missing runtime included, and
//! so do the tests of the runtime selection of `rusteda-compute`. `--gpu` builds the command line
//! with every GPU runtime and adds its ignored GPU tests, and runs the device self-test of
//! `rusteda-compute` on every runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::pdk::manifest::{self, Fields};

/// Options of `verify`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub(crate) gpu: bool,
}

/// One `[[suite]]` entry in `verification/suites/<domain>/*.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Suite {
    pub(crate) file: String,
    pub(crate) name: String,
    pub(crate) requirement: Vec<String>,
    pub(crate) test: String,
    pub(crate) cases: Option<String>,
    pub(crate) description: Option<String>,
}

/// The integration-test targets of the command-line crate that `verify sta` runs.
pub(crate) const CLI_SUITES: [&str; 5] = [
    "sta_known_answer",
    "sta_properties",
    "sta_inputs",
    "sta_view_model",
    "cli_contract",
];

/// The GPU runtimes of `--gpu`: features of the command-line crate and of `rusteda-compute`.
pub(crate) const GPU_FEATURES: &str = "cuda";

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Sorted `*.toml` files inside `dir`.
fn sorted_toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    Ok(files)
}

/// Parses one suite file's text into [`Suite`] entries.
pub(crate) fn parse_suites(path: &str, text: &str) -> Result<Vec<Suite>, String> {
    let mut out = Vec::new();
    for t in manifest::parse(path, text)? {
        if t.name != "suite" {
            return Err(format!(
                "{path}:{}: unknown table [[{}]] (expected [[suite]])",
                t.line, t.name
            ));
        }
        let mut f = Fields::new(path, &t);
        let name = f.str("name")?;
        let requirement = f
            .opt_list("requirement")?
            .ok_or_else(|| f.err("missing 'requirement'"))?;
        let test = f.str("test")?;
        let cases = f.opt_str("cases")?;
        let description = f.opt_str("description")?;
        if !manifest::plain_name(&name) {
            return Err(f.err(&format!("'name' must be a plain name: '{name}'")));
        }
        if requirement.is_empty() {
            return Err(f.err("'requirement' must not be empty"));
        }
        if !manifest::plain_path(&test) {
            return Err(f.err(&format!("'test' is not a plain relative path: '{test}'")));
        }
        if let Some(c) = &cases
            && !manifest::plain_path(c)
        {
            return Err(f.err(&format!("'cases' is not a plain relative path: '{c}'")));
        }
        f.done()?;
        out.push(Suite {
            file: path.to_string(),
            name,
            requirement,
            test,
            cases,
            description,
        });
    }
    if out.is_empty() {
        return Err(format!("{path}: no [[suite]]"));
    }
    Ok(out)
}

/// Loads all `verification/suites/<domain>/*.toml` files in deterministic sorted path order.
pub(crate) fn load_domain(root: &Path, domain: &str) -> Result<Vec<Suite>, String> {
    let dir = root.join("verification/suites").join(domain);
    let mut out = Vec::new();
    let mut seen_names = BTreeMap::new();
    for p in sorted_toml_files(&dir)? {
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        for s in parse_suites(&rel, &text)? {
            if let Some(prev) = seen_names.insert(s.name.clone(), s.file.clone()) {
                return Err(format!(
                    "{}: duplicate suite '{}' (already in {prev})",
                    s.file, s.name
                ));
            }
            out.push(s);
        }
    }
    Ok(out)
}

/// Extracts `(package, test_target)` when `test_path` is `crates/<group>/<package>/tests/<target>.rs`.
fn integration_target(test_path: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = test_path.split('/').collect();
    match parts.as_slice() {
        ["crates", _, pkg, "tests", file] => file.strip_suffix(".rs").map(|t| (*pkg, t)),
        _ => None,
    }
}

fn target_exists(root: &Path, pkg_dir: &Path, target: &str) -> bool {
    if !root.join("crates").is_dir() {
        return true;
    }
    let tests = pkg_dir.join("tests");
    tests.join(format!("{target}.rs")).is_file() || tests.join(target).join("main.rs").is_file()
}

fn source_path(root: &Path, common: Option<&Path>, relative: &str) -> PathBuf {
    let local = root.join(relative);
    if local.is_file() {
        return local;
    }
    let shared = crate::shared::CRATES
        .iter()
        .any(|(group, package)| relative.starts_with(&format!("crates/{group}/{package}/")));
    if shared && let Some(common) = common {
        return common.join(relative);
    }
    local
}

fn plan_sta(root: &Path, o: Options) -> Result<Vec<Vec<String>>, String> {
    let common = crate::foundation::root(root)?;
    let suites = load_domain(root, "sta")?;
    let cli_dir = root.join("crates/apps/ruststa-cli");
    let mut cli_targets: Vec<String> = CLI_SUITES
        .iter()
        .copied()
        .filter(|t| target_exists(root, &cli_dir, t))
        .map(str::to_string)
        .collect();
    let mut extra_crates: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &suites {
        if let Some((pkg, target)) = integration_target(&s.test) {
            if root.join("crates").is_dir()
                && !source_path(root, common.as_deref(), &s.test).is_file()
            {
                continue;
            }
            if pkg == "ruststa-cli" {
                if !cli_targets.iter().any(|t| t == target) {
                    cli_targets.push(target.to_string());
                }
            } else {
                let list = extra_crates.entry(pkg.to_string()).or_default();
                if !list.iter().any(|t| t == target) {
                    list.push(target.to_string());
                }
            }
        }
    }
    let mut a = args(&["test", "--release", "-p", "ruststa-cli"]);
    if o.gpu {
        a.extend(args(&["--features", GPU_FEATURES]));
    } else {
        a.push("--no-default-features".into());
    }
    a.push("--lib".into());
    for t in &cli_targets {
        a.push("--test".into());
        a.push(t.clone());
    }
    if o.gpu {
        a.extend(args(&["--", "--include-ignored"]));
    }
    let mut steps = vec![a];
    for (pkg, targets) in extra_crates {
        let mut step = args(&["test", "--release", "-p", &pkg]);
        if o.gpu && pkg == "ruststa" {
            step.extend(args(&["--features", GPU_FEATURES]));
        }
        for t in &targets {
            step.push("--test".into());
            step.push(t.clone());
        }
        steps.push(step);
        if o.gpu && pkg == "ruststa" && targets.iter().any(|t| t == "backend_equality") {
            steps.push(args(&[
                "test",
                "--release",
                "-p",
                "ruststa",
                "--features",
                GPU_FEATURES,
                "--test",
                "backend_equality",
                "--",
                "--ignored",
            ]));
        }
    }
    if o.gpu {
        steps.push(args(&[
            "test",
            "--release",
            "-p",
            "rusteda-compute",
            "--features",
            GPU_FEATURES,
            "--lib",
            "--",
            "--ignored",
        ]));
    } else {
        steps.push(args(&[
            "test",
            "--release",
            "-p",
            "rusteda-compute",
            "--lib",
        ]));
    }
    Ok(steps)
}

fn plan_compute(root: &Path, o: Options) -> Result<Vec<Vec<String>>, String> {
    let common = crate::foundation::root(root)?;
    let suites = load_domain(root, "compute")?;
    let mut steps = Vec::new();
    if o.gpu {
        steps.push(args(&[
            "test",
            "--release",
            "-p",
            "rusteda-compute",
            "--features",
            GPU_FEATURES,
            "--lib",
            "--",
            "--include-ignored",
        ]));
    } else {
        steps.push(args(&[
            "test",
            "--release",
            "-p",
            "rusteda-compute",
            "--lib",
        ]));
    }
    let mut extra: Vec<String> = Vec::new();
    for s in &suites {
        if let Some(("rusteda-compute", target)) = integration_target(&s.test)
            && (!root.join("crates").is_dir()
                || source_path(root, common.as_deref(), &s.test).is_file())
            && !extra.iter().any(|t| t == target)
        {
            extra.push(target.to_string());
        }
    }
    if !extra.is_empty() {
        let mut step = args(&["test", "--release", "-p", "rusteda-compute"]);
        if o.gpu {
            step.extend(args(&["--features", GPU_FEATURES]));
        }
        for t in extra {
            step.push("--test".into());
            step.push(t);
        }
        steps.push(step);
    }
    Ok(steps)
}

fn plan_sdc(root: &Path) -> Result<Vec<Vec<String>>, String> {
    let common = crate::foundation::root(root)?;
    let suites = load_domain(root, "sdc")?;
    let mut by_pkg: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &suites {
        if let Some((pkg, target)) = integration_target(&s.test)
            && (!root.join("crates").is_dir()
                || source_path(root, common.as_deref(), &s.test).is_file())
        {
            let list = by_pkg.entry(pkg.to_string()).or_default();
            if !list.iter().any(|t| t == target) {
                list.push(target.to_string());
            }
        }
    }
    if by_pkg.is_empty() {
        return Ok(vec![args(&[
            "test",
            "--release",
            "-p",
            "rusteda-formats",
            "--lib",
            "sdc",
        ])]);
    }
    let mut steps = Vec::new();
    for (pkg, targets) in by_pkg {
        let mut step = args(&["test", "--release", "-p", &pkg]);
        for t in targets {
            step.push("--test".into());
            step.push(t);
        }
        steps.push(step);
    }
    Ok(steps)
}

/// The cargo invocations of `product` in the workspace at `root`, in order.
pub(crate) fn plan_in(root: &Path, product: &str, o: Options) -> Result<Vec<Vec<String>>, String> {
    match product {
        "sta" => plan_sta(root, o),
        "compute" => plan_compute(root, o),
        "sdc" => plan_sdc(root),
        "all" => {
            let mut steps = plan_sta(root, o)?;
            for s in plan_sdc(root)?.into_iter().chain(plan_compute(root, o)?) {
                if !steps.contains(&s) {
                    steps.push(s);
                }
            }
            Ok(steps)
        }
        _ => Err(format!("unknown product '{product}'")),
    }
}

/// The cargo invocations of `product`, in order.
#[cfg(test)]
pub(crate) fn plan(product: &str, o: Options) -> Result<Vec<Vec<String>>, String> {
    plan_in(&crate::root(), product, o)
}

/// The `verify` command: runs every step of the plan (all of them, also after a failure).
pub(crate) fn run(root: &Path, product: &str, o: Options) -> Result<(), String> {
    let mut failed = Vec::new();
    let guard = crate::guard::Guard::detect();
    println!("verify: {}", guard.describe());
    for a in &plan_in(root, product, o)? {
        let ok = guard
            .cargo(env!("CARGO"), a)
            .current_dir(root)
            .status()
            .map_err(|e| format!("cargo {}: {e}", a.join(" ")))?
            .success();
        if !ok {
            failed.push(format!("cargo {}", a.join(" ")));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "verification suites failed:\n  {}",
            failed.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests;
