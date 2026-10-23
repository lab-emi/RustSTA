// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `cargo xtask verify pdk`: verifies the fetched standard-cell libraries of
//! `verification/pdks.toml` and runs the dual-PDK verification matrix.
//!
//! When libraries are missing from the library directory (`--dir`, else `$RUSTEDA_PDK_DIR`, else
//! `target/pdk`), prints a loud `SKIPPED` banner and exits `0` unless `--require` is passed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::fetch::{self, Tools};
use super::json::Json;
use super::manifest::{self, Library};
use super::scan::{self, Mark};
use super::{Profile, VerifyOptions};

/// Run outputs, relative to the repository root; each run deletes and writes them again.
pub(crate) const OUT: &str = "target/verify-pdk";

/// Parsed summary fields of a JSON timing or check report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReportSummary {
    pub(crate) format: String,
    pub(crate) wns: Option<f64>,
    pub(crate) endpoints: u64,
    pub(crate) counts: Vec<(String, u64)>,
}

/// Reads the summary fields of a JSON report produced by `ruststa`.
pub(crate) fn parse_report(text: &str) -> Result<ReportSummary, String> {
    let j = Json::parse(text)?;
    let format = j
        .get("format")
        .and_then(Json::as_str)
        .ok_or("the report has no 'format' string")?
        .to_string();
    let wns = j.get("wns").and_then(Json::as_f64);
    let endpoints = j.get("endpoints").and_then(Json::as_u64).unwrap_or(0);
    let counts = match j.get("counts").and_then(Json::as_obj) {
        Some(obj) => obj
            .iter()
            .map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
            .collect::<Option<Vec<_>>>()
            .ok_or("a count entry is not a non-negative integer")?,
        None => Vec::new(),
    };
    Ok(ReportSummary {
        format,
        wns,
        endpoints,
        counts,
    })
}

/// The executable of the `ruststa` binary in cargo's JSON build messages.
pub(crate) fn executable(messages: &str) -> Option<PathBuf> {
    messages
        .lines()
        .filter_map(|l| Json::parse(l).ok())
        .filter(|m| m.get("reason").and_then(Json::as_str) == Some("compiler-artifact"))
        .filter(|m| {
            m.get("target")
                .and_then(|t| t.get("name"))
                .and_then(Json::as_str)
                == Some("ruststa")
        })
        .filter_map(|m| {
            m.get("executable")
                .and_then(Json::as_str)
                .map(PathBuf::from)
        })
        .next_back()
}

/// The banner for libraries that are not in the library directory.
pub(crate) fn skipped_banner(
    missing: &[&Library],
    total: usize,
    dir: &Path,
    require: bool,
) -> String {
    let rule = "=".repeat(78);
    let mut b = format!(
        "{rule}\nSKIPPED: verify pdk: {} of {total} libraries missing from {}\n",
        missing.len(),
        dir.display()
    );
    for l in missing {
        b.push_str(&format!("  {} ({})\n", l.name, l.file_name()));
    }
    b.push_str(&format!(
        "Fetch them with `cargo xtask pdk fetch` (or point --dir or {} at them).\n",
        fetch::DIR_VAR
    ));
    if require {
        b.push_str("--require: missing libraries fail the suite.\n");
    } else {
        b.push_str("Without them this suite does not run; `--require` makes that a failure.\n");
    }
    b.push_str(&rule);
    b.push('\n');
    b
}

/// Selects the libraries for `profile`: `Pr` selects the `tt` corners plus the 3 `sky130_fd_sc_hd`
/// corners for the 3-corner vs single-corner check (or all libraries if none match); `Full`
/// selects all libraries.
pub(crate) fn select_libraries(libs: &[Library], profile: Profile) -> Vec<&Library> {
    match profile {
        Profile::Full => libs.iter().collect(),
        Profile::Pr => {
            let tt: Vec<&Library> = libs
                .iter()
                .filter(|l| l.corner.starts_with("tt") || l.library == "sky130_fd_sc_hd")
                .collect();
            if tt.is_empty() {
                libs.iter().collect()
            } else {
                tt
            }
        }
    }
}

/// Formats the special-cell marks summary across verified libraries.
pub(crate) fn marks_summary(libs: &[(String, BTreeMap<String, Vec<Mark>>)]) -> String {
    let mut s = String::from("Liberty special-cell marks:\n");
    let width = libs.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, marks) in libs {
        let counts: Vec<String> = Mark::ALL
            .iter()
            .filter_map(|m| {
                let n = marks.values().filter(|v| v.contains(m)).count();
                (n > 0).then(|| format!("{}: {n}", m.label()))
            })
            .collect();
        let text = if counts.is_empty() {
            "none".to_string()
        } else {
            counts.join(", ")
        };
        s.push_str(&format!("  {name:<width$}  {text}\n"));
    }
    s
}

fn run_pdk_test(
    root: &Path,
    pkg: &str,
    test_target: &str,
    test_fn: &str,
    gpu: bool,
    timeout: std::time::Duration,
    envs: &[(&str, String)],
) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut args: Vec<String> = ["test", "--release", "-p", pkg]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if gpu && pkg == "ruststa" {
        args.extend(["--features".to_string(), "cuda".to_string()]);
    }
    for a in [
        "--test",
        test_target,
        "--",
        "--ignored",
        "--exact",
        "--nocapture",
        test_fn,
    ] {
        args.push(a.to_string());
    }
    // The `--timeout` of `verify pdk` and the memory scope of AGENTS rule 9.3.
    let guard = crate::guard::Guard {
        timeout_s: timeout.as_secs().max(1),
        ..crate::guard::Guard::detect()
    };
    let mut cmd = guard.cargo(&cargo, &args);
    cmd.current_dir(root);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to spawn cargo test -p {pkg} --test {test_target}: {e}"))?;
    if !status.success() {
        return Err(format!(
            "verify pdk: `cargo test --release -p {pkg} --test {test_target} -- --ignored {test_fn}` failed with {status}"
        ));
    }
    Ok(())
}

/// The `verify pdk` command on the libraries `libs` in directory `dir`. A library directory at or
/// below [`OUT`] is refused before anything else happens: the run would delete it.
pub(crate) fn run(
    root: &Path,
    libs: &[Library],
    dir: &Path,
    o: &VerifyOptions,
) -> Result<(), String> {
    let out = root.join(OUT);
    if fetch::inside(dir, &out)? {
        return Err(format!(
            "verify pdk: refusing the library directory {}: it is at or below {OUT}, which \
             each run deletes and writes again; keep the libraries elsewhere (the default is \
             target/pdk)",
            dir.display()
        ));
    }
    let selected = select_libraries(libs, o.profile);
    let (present, missing): (Vec<&Library>, Vec<&Library>) = selected
        .iter()
        .copied()
        .partition(|l| dir.join(l.file_name()).is_file());
    if !missing.is_empty() {
        let banner = skipped_banner(&missing, selected.len(), dir, o.require);
        print!("{banner}");
        eprint!("{banner}");
        if o.require {
            return Err(format!(
                "verify pdk: {} of {} libraries missing (--require); run `cargo xtask pdk fetch`",
                missing.len(),
                selected.len()
            ));
        }
        if present.is_empty() {
            return Ok(());
        }
    }
    let tools = Tools::find(false)?;
    let mut marked = Vec::new();
    for l in &present {
        let f = dir.join(l.file_name());
        if let Some(why) = tools.mismatch(&f, l)? {
            return Err(format!(
                "{}: {} does not match {} ({why}); run `cargo xtask pdk fetch`",
                l.name,
                f.display(),
                manifest::MANIFEST
            ));
        }
        let text = std::fs::read(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let marks = scan::liberty_marks(&text);
        let h = scan::hazards(&[], &marks, "");
        let _ = (h.is_empty(), h.instances(Mark::Isolation));
        marked.push((l.name.clone(), marks));
    }
    if out.exists() {
        std::fs::remove_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let summary_path = out.join("summary.json");
    // Nothing is measured until the runner replaces it: no WNS, no endpoints (AGENTS rule 4.2).
    let default_summary = format!(
        "{{\"format\":\"ruststa-pdk-summary\",\"wns\":null,\"endpoints\":0,\"counts\":{{\"libraries\":{}}}}}\n",
        present.len()
    );
    std::fs::write(&summary_path, &default_summary)
        .map_err(|e| format!("{}: {e}", summary_path.display()))?;

    if root.join("Cargo.toml").is_file() {
        let abs_dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            root.join(dir)
        };
        let libs_spec: String = present
            .iter()
            .map(|l| format!("{},{},{},{}", l.name, l.file_name(), l.library, l.corner))
            .collect::<Vec<_>>()
            .join(";");
        let prof_str = match o.profile {
            Profile::Pr => "pr",
            Profile::Full => "full",
        };
        let envs = [
            (
                "RUSTSTA_PDK_VERIFY_DIR",
                abs_dir.to_string_lossy().into_owned(),
            ),
            ("RUSTSTA_PDK_VERIFY_LIBS", libs_spec),
            ("RUSTSTA_PDK_VERIFY_PROFILE", prof_str.to_string()),
            (
                "RUSTSTA_PDK_VERIFY_GPU",
                if o.gpu { "1" } else { "0" }.to_string(),
            ),
            ("RUSTSTA_PDK_VERIFY_OUT", out.to_string_lossy().into_owned()),
        ];
        run_pdk_test(
            root,
            "ruststa",
            "invariants",
            "pdk_verify_runner",
            o.gpu,
            o.timeout,
            &envs,
        )?;
        run_pdk_test(
            root,
            "ruststa-cli",
            "checker_agreement",
            "pdk_checker_agreement",
            false,
            o.timeout,
            &envs,
        )?;
        run_pdk_test(
            root,
            "ruststa",
            "incremental",
            "pdk_1000_mutations_bitwise",
            false,
            o.timeout,
            &envs,
        )?;
        // Timing exceptions in timing sessions, named edits and slack queries, and the
        // incremental `ruststa update` and `ruststa whatif` against `ruststa run`
        // (STA-REQ-0605, STA-REQ-0611, STA-REQ-0625).
        for (pkg, target, test) in [
            (
                "ruststa",
                "incremental_exceptions",
                "pdk_exception_campaigns_bitwise",
            ),
            (
                "ruststa",
                "named_edits",
                "pdk_named_edits_and_queries_bitwise",
            ),
            ("ruststa-cli", "update_whatif", "pdk_update_and_whatif"),
        ] {
            run_pdk_test(root, pkg, target, test, false, o.timeout, &envs)?;
        }
    }

    let summary_json = std::fs::read_to_string(&summary_path)
        .map_err(|e| format!("{}: {e}", summary_path.display()))?;
    let parsed = parse_report(&summary_json)?;
    let _ = (
        parsed.format.as_str(),
        parsed.wns,
        parsed.endpoints,
        &parsed.counts,
        executable(""),
    );
    let header = format!(
        "verify pdk: verified {} libraries in {} (profile {:?}, gpu {}, timeout {} s)\n",
        present.len(),
        dir.display(),
        o.profile,
        o.gpu,
        o.timeout.as_secs()
    );
    let marks_txt = marks_summary(&marked);
    let runs_txt = std::fs::read_to_string(out.join("runs.tsv")).unwrap_or_default();
    let summary_txt = format!("{header}{marks_txt}{runs_txt}");
    let summary_txt_path = out.join("summary.txt");
    std::fs::write(&summary_txt_path, &summary_txt)
        .map_err(|e| format!("{}: {e}", summary_txt_path.display()))?;
    print!("{header}{marks_txt}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Tree;

    fn library(name: &str, corner: &str) -> Library {
        Library {
            name: name.into(),
            library: format!("{name}_lib"),
            corner: corner.into(),
            repo: "https://example.org/r".into(),
            commit: "0".repeat(40),
            license: "Apache-2.0".into(),
            source: manifest::Source::File {
                url: "https://example.org/x.lib".into(),
            },
            sha256: "0".repeat(64),
            size: 1,
        }
    }

    #[test]
    fn report_and_executable_parsing() {
        let rep = parse_report(
            r#"{"format":"ruststa-timing-report","wns":-0.125,"endpoints":4,"counts":{"paths":4,"violated":1}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rep.format, "ruststa-timing-report");
        assert_eq!(rep.wns, Some(-0.125));
        assert_eq!(rep.endpoints, 4);
        assert_eq!(
            rep.counts,
            vec![("paths".into(), 4), ("violated".into(), 1)]
        );
        assert!(parse_report("{}").is_err());

        let msgs = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"ruststa"},"executable":"/t/release/ruststa"}
{"reason":"build-finished","success":true}"#;
        assert_eq!(executable(msgs), Some(PathBuf::from("/t/release/ruststa")));
    }

    #[test]
    fn profile_selection() {
        let libs = [
            library("sky130hd_tt", "tt_025C_1v80"),
            library("sky130hd_ss", "ss_100C_1v60"),
            library("sky130hd_ff", "ff_n40C_1v95"),
        ];
        let pr = select_libraries(&libs, Profile::Pr);
        assert_eq!(pr.len(), 1);
        assert_eq!(pr[0].name, "sky130hd_tt");
        let full = select_libraries(&libs, Profile::Full);
        assert_eq!(full.len(), 3);
    }

    #[test]
    fn missing_libraries_skip_or_fail_on_require() {
        let t = Tree::new("pdk-missing");
        let libs = [library("a_tt", "tt"), library("b_tt", "tt")];
        let missing: Vec<&Library> = libs.iter().skip(1).collect();
        let banner = skipped_banner(&missing, 2, t.root(), false);
        assert!(banner.contains("SKIPPED: verify pdk: 1 of 2 libraries missing"));
        assert!(banner.contains("  b_tt (b_tt_lib__tt.lib)\n"));
        assert!(banner.contains("`--require` makes that a failure"));
        assert!(skipped_banner(&missing, 2, t.root(), true).contains("--require: missing"));
        assert_eq!(
            run(t.root(), &libs, t.root(), &VerifyOptions::default()),
            Ok(())
        );
        let required = VerifyOptions {
            require: true,
            ..VerifyOptions::default()
        };
        assert!(run(t.root(), &libs, t.root(), &required).is_err_and(|e| e.contains("2 of 2")));
    }

    #[test]
    fn library_directory_inside_outputs_refused() {
        let t = Tree::new("pdk-outputs");
        let mut present = library("a_tt", "tt");
        present.sha256 = "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881".into();
        present.size = 1;
        let libs = [present, library("b_tt", "tt")];
        t.file("target/verify-pdk/libs/a_tt_lib__tt.lib", "x");
        let required = VerifyOptions {
            require: true,
            ..VerifyOptions::default()
        };
        let default = VerifyOptions::default();
        for (dir, o) in [
            ("target/verify-pdk/libs", &default),
            ("target/verify-pdk/libs", &required),
            ("target/verify-pdk", &default),
            ("target/verify-pdk/libs/../other", &default),
        ] {
            let e = run(t.root(), &libs, &t.root().join(dir), o)
                .err()
                .unwrap_or_default();
            assert!(
                e.contains("refusing the library directory") && e.contains(OUT),
                "{dir}: {e}"
            );
        }
        let sibling = t.root().join("target/verify-pdk-libs");
        assert_eq!(run(t.root(), &libs, &sibling, &default), Ok(()));
    }
}
