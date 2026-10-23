// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Runs every `console` fenced block (excluding `console,lab`) from `README.md`,
//! `docs/user/sta/README.md`, and `docs/user/cli/README.md` verbatim against a temporary
//! workspace under `CARGO_TARGET_TMPDIR` with synthetic fixtures and verifies exit statuses
//! and JSON envelope shapes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

const COUNTER_SDC: &str = "\
create_clock -name clk -period 0.5 [get_ports clk_i]
set_output_delay 0.1 -clock clk [all_outputs]
";

/// A single `$ <command>` invocation parsed from a `console` block, with its shown output lines.
#[derive(Debug, Clone)]
struct ConsoleStep {
    file: String,
    block_idx: usize,
    cmd_line: String,
    argv: Vec<String>,
    expected_output: String,
}

/// Temporary directory rooted under `CARGO_TARGET_TMPDIR` that removes itself on drop.
struct TempWorkDir {
    path: PathBuf,
}

impl TempWorkDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let pid = std::process::id();
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("docs-examples-{tag}-{pid}-{n}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp work dir under CARGO_TARGET_TMPDIR");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf()
}

fn ruststa_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ruststa"))
}

fn rusteda_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let sta = ruststa_bin();
        let dir = sta.parent().expect("target debug dir");
        let exe = format!("rusteda{}", std::env::consts::EXE_SUFFIX);
        let bin = dir.join(exe);
        if !bin.is_file() {
            let status = Command::new(env!("CARGO"))
                .current_dir(repo_root())
                .args(["build", "--quiet", "-p", "rusteda-cli", "--bin", "rusteda"])
                .status()
                .expect("build rusteda binary");
            assert!(status.success(), "cargo build -p rusteda-cli failed");
        }
        assert!(
            bin.is_file(),
            "rusteda binary not found at {}",
            bin.display()
        );
        bin
    })
}

/// Checks whether the compiled `ruststa` binary has the workspace subcommands (`run`, `update`,
/// `check`, `path`, `inspect`, `report`).
fn ruststa_has_workspace_cli() -> bool {
    static HAS: OnceLock<bool> = OnceLock::new();
    *HAS.get_or_init(|| {
        let out = Command::new(ruststa_bin())
            .arg("--help")
            .output()
            .expect("run ruststa --help");
        let help = String::from_utf8_lossy(&out.stdout);
        help.contains("run") && help.contains("--workspace")
    })
}

/// Stages synthetic fixtures into `dir` so short fixture names (`wt_full.lib`, `counter.v`,
/// `counter.sdc`, `chain_8.v`, `chain_8.sdc`, etc.) resolve directly in `dir`.
fn stage_synthetic_fixtures(dir: &Path) {
    let root = repo_root();
    let sta_fix = root.join("verification/fixtures/synthetic/sta");
    fs::copy(sta_fix.join("lib/wt_full.lib"), dir.join("wt_full.lib")).expect("copy wt_full.lib");
    fs::copy(sta_fix.join("lib/wt_15.lib"), dir.join("wt_15.lib")).expect("copy wt_15.lib");
    fs::copy(sta_fix.join("lib/wt_full.lib"), dir.join("cells.lib")).expect("copy cells.lib");
    fs::copy(sta_fix.join("netlists/counter.v"), dir.join("counter.v")).expect("copy counter.v");
    fs::copy(sta_fix.join("netlists/counter.v"), dir.join("top.v")).expect("copy top.v");
    fs::write(dir.join("counter.sdc"), COUNTER_SDC).expect("write counter.sdc");
    fs::write(dir.join("top.sdc"), COUNTER_SDC).expect("write top.sdc");
    fs::copy(sta_fix.join("net/chain_8.v"), dir.join("chain_8.v")).expect("copy chain_8.v");
    fs::copy(sta_fix.join("sdc/chain_8.sdc"), dir.join("chain_8.sdc")).expect("copy chain_8.sdc");
}

/// Splits a shell command line into words, honoring single quotes, double quotes, and backslash
/// escapes.
fn split_shell_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' if !in_word => {}
            ' ' | '\t' => {
                words.push(std::mem::take(&mut cur));
                in_word = false;
            }
            '\'' => {
                in_word = true;
                for ch in chars.by_ref() {
                    if ch == '\'' {
                        break;
                    }
                    cur.push(ch);
                }
            }
            '"' => {
                in_word = true;
                while let Some(ch) = chars.next() {
                    if ch == '"' {
                        break;
                    }
                    if ch == '\\' {
                        if let Some(next) = chars.next() {
                            cur.push(next);
                        }
                    } else {
                        cur.push(ch);
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            other => {
                in_word = true;
                cur.push(other);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    if words.is_empty() {
        return Err("empty command line".to_string());
    }
    Ok(words)
}

/// Extracts all fenced code blocks with info string `tag` from `markdown`.
fn extract_fenced_blocks(markdown: &str, tag: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut active_match = false;
    let mut buf = Vec::new();

    for line in markdown.lines() {
        let trimmed = line.trim_end();
        if !in_block {
            if let Some(rest) = trimmed.strip_prefix("```") {
                in_block = true;
                active_match = rest.trim() == tag;
                buf.clear();
            }
        } else if trimmed == "```" {
            if active_match {
                blocks.push(buf.join("\n"));
            }
            in_block = false;
            active_match = false;
            buf.clear();
        } else if active_match {
            buf.push(line);
        }
    }
    blocks
}

/// Parses a `console` block into ordered [`ConsoleStep`] invocations.
fn parse_console_block(rel_file: &str, block_idx: usize, block: &str) -> Vec<ConsoleStep> {
    let lines: Vec<&str> = block.lines().collect();
    let mut steps = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        if let Some(first) = line.strip_prefix("$ ") {
            let mut cmd = first.trim_end().to_string();
            while cmd.ends_with('\\') {
                cmd.pop();
                cmd = cmd.trim_end().to_string();
                i += 1;
                assert!(
                    i < lines.len(),
                    "{rel_file} block #{block_idx}: trailing backslash at end of block"
                );
                cmd.push(' ');
                cmd.push_str(lines[i].trim());
            }
            i += 1;
            let mut out_lines = Vec::new();
            while i < lines.len() && !lines[i].starts_with("$ ") {
                out_lines.push(lines[i]);
                i += 1;
            }
            let argv = split_shell_words(&cmd)
                .unwrap_or_else(|e| panic!("{rel_file} block #{block_idx}: {e} in `{cmd}`"));
            steps.push(ConsoleStep {
                file: rel_file.to_string(),
                block_idx,
                cmd_line: cmd,
                argv,
                expected_output: out_lines.join("\n").trim().to_string(),
            });
        } else {
            panic!("{rel_file} block #{block_idx}: unexpected line before `$ ` prompt: `{line}`");
        }
    }
    steps
}

/// Checks that every key/value in `expected` matches the shape and fixed envelope fields of
/// `actual`.
fn assert_envelope_shape(ctx: &str, expected: &Value, actual: &Value) {
    let exp_obj = expected
        .as_object()
        .unwrap_or_else(|| panic!("{ctx}: expected JSON must be an object"));
    let act_obj = actual
        .as_object()
        .unwrap_or_else(|| panic!("{ctx}: actual stdout must be a JSON object: {actual}"));

    for required in [
        "schema",
        "ok",
        "command",
        "workspace",
        "base_rev",
        "rev",
        "new_rev",
        "scope",
        "completeness",
        "metrics",
        "diagnostics",
        "error",
        "next_actions",
    ] {
        assert!(
            act_obj.contains_key(required),
            "{ctx}: actual envelope missing required key `{required}`: {actual}"
        );
    }
    assert_eq!(
        act_obj.get("schema").and_then(Value::as_str),
        Some("rusteda.v1"),
        "{ctx}: schema must be rusteda.v1"
    );

    for exact_key in ["schema", "ok", "command", "base_rev", "rev", "new_rev"] {
        if let Some(exp_val) = exp_obj.get(exact_key) {
            assert_eq!(
                act_obj.get(exact_key),
                Some(exp_val),
                "{ctx}: envelope field `{exact_key}` mismatch"
            );
        }
    }

    for (k, exp_v) in exp_obj {
        let act_v = act_obj
            .get(k)
            .unwrap_or_else(|| panic!("{ctx}: actual envelope missing key `{k}`"));
        assert_value_shape(&format!("{ctx}.{k}"), exp_v, act_v);
    }
}

fn assert_value_shape(ctx: &str, expected: &Value, actual: &Value) {
    match (expected, actual) {
        (Value::Object(exp_map), Value::Object(act_map)) => {
            for (k, exp_v) in exp_map {
                let act_v = act_map
                    .get(k)
                    .unwrap_or_else(|| panic!("{ctx}: missing object key `{k}` in {actual}"));
                assert_value_shape(&format!("{ctx}.{k}"), exp_v, act_v);
            }
        }
        (Value::Array(exp_arr), Value::Array(act_arr)) => {
            if !exp_arr.is_empty() {
                assert!(
                    act_arr.len() >= exp_arr.len(),
                    "{ctx}: expected at least {} array items, got {}",
                    exp_arr.len(),
                    act_arr.len()
                );
                for (idx, exp_item) in exp_arr.iter().enumerate() {
                    assert_value_shape(&format!("{ctx}[{idx}]"), exp_item, &act_arr[idx]);
                }
            }
        }
        (Value::Bool(e), Value::Bool(a)) => assert_eq!(a, e, "{ctx}: boolean mismatch"),
        (Value::Null, Value::Null) => {}
        (Value::String(e), Value::String(a)) => {
            if ctx.ends_with(".handle") || ctx.ends_with(".kind") || ctx.ends_with(".corner") {
                assert_eq!(a, e, "{ctx}: string value mismatch");
            }
        }
        (Value::Number(_), Value::Number(_)) => {}
        _ => panic!("{ctx}: JSON type mismatch: expected {expected}, got {actual}"),
    }
}

/// Validates that an expected JSON block in the markdown file has the canonical `rusteda.v1`
/// envelope keys.
fn validate_documented_envelope(ctx: &str, expected_json: &str) -> Value {
    let v: Value = serde_json::from_str(expected_json)
        .unwrap_or_else(|e| panic!("{ctx}: invalid JSON in console block: {e}\n{expected_json}"));
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("{ctx}: documented JSON must be an object"));
    assert_eq!(
        obj.get("schema").and_then(Value::as_str),
        Some("rusteda.v1"),
        "{ctx}: documented schema must be rusteda.v1"
    );
    for key in [
        "schema",
        "ok",
        "command",
        "workspace",
        "base_rev",
        "rev",
        "new_rev",
        "scope",
        "completeness",
        "metrics",
        "diagnostics",
        "error",
        "next_actions",
    ] {
        assert!(
            obj.contains_key(key),
            "{ctx}: documented envelope missing key `{key}`"
        );
    }
    v
}

/// Extracts `-w` / `--workspace` and `--base-rev` values from an argument list.
fn extract_ws_and_base_rev(argv: &[String]) -> (Option<String>, Option<String>) {
    let mut ws = None;
    let mut base_rev = None;
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-w" | "--workspace" => {
                ws = argv.get(i + 1).cloned();
                i += 2;
            }
            "--base-rev" => {
                base_rev = argv.get(i + 1).cloned();
                i += 2;
            }
            s if s.starts_with("--workspace=") => {
                ws = s.strip_prefix("--workspace=").map(String::from);
                i += 1;
            }
            s if s.starts_with("--base-rev=") => {
                base_rev = s.strip_prefix("--base-rev=").map(String::from);
                i += 1;
            }
            _ => i += 1,
        }
    }
    (ws, base_rev)
}

fn run_doc_file(rel_file: &str, tag: &str) {
    let path = repo_root().join(rel_file);
    let markdown = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let blocks = extract_fenced_blocks(&markdown, "console");
    assert!(
        !blocks.is_empty(),
        "{rel_file} must contain at least one ```console block"
    );

    let work = TempWorkDir::new(tag);
    stage_synthetic_fixtures(work.path());

    let has_sta_ws = ruststa_has_workspace_cli();

    for (block_idx, block) in blocks.iter().enumerate() {
        let steps = parse_console_block(rel_file, block_idx, block);
        assert!(
            !steps.is_empty(),
            "{rel_file} block #{block_idx} must contain at least one `$ ` command"
        );

        for step in &steps {
            let ctx = format!(
                "{}:block#{}: `{}`",
                step.file, step.block_idx, step.cmd_line
            );
            let prog = step.argv[0].as_str();
            assert!(
                prog == "rusteda" || prog == "ruststa",
                "{ctx}: console command must invoke `rusteda` or `ruststa`, got `{prog}`"
            );

            let expected_val = if step.expected_output.starts_with('{') {
                Some(validate_documented_envelope(&ctx, &step.expected_output))
            } else {
                None
            };

            if prog == "ruststa" && !has_sta_ws {
                // Before K1 lands on this branch, advance the workspace revision for mutations that
                // create a new revision so subsequent `rusteda` commands in the same document
                // execute against the expected revision numbers.
                let sub = step.argv.get(1).map(String::as_str).unwrap_or("");
                let creates_rev = expected_val
                    .as_ref()
                    .and_then(|v| v.get("new_rev"))
                    .is_some_and(Value::is_string)
                    || (expected_val.is_none() && matches!(sub, "run" | "check"));
                if creates_rev
                    && let (Some(ws), Some(base_rev)) = extract_ws_and_base_rev(&step.argv)
                {
                    let out = Command::new(rusteda_bin())
                        .current_dir(work.path())
                        .args([
                            "import",
                            "-w",
                            &ws,
                            "--base-rev",
                            &base_rev,
                            "--sdc",
                            "counter.sdc",
                        ])
                        .output()
                        .expect("advance workspace revision for pre-K1 fallback");
                    assert!(
                        out.status.success(),
                        "{ctx}: pre-K1 revision advance failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                continue;
            }

            let bin = if prog == "rusteda" {
                rusteda_bin().to_path_buf()
            } else {
                ruststa_bin()
            };

            let out = Command::new(&bin)
                .current_dir(work.path())
                .args(&step.argv[1..])
                .output()
                .unwrap_or_else(|e| panic!("{ctx}: failed to execute {}: {e}", bin.display()));

            assert!(
                out.status.success(),
                "{ctx}: command exited with {:?}\nstdout:\n{}\nstderr:\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );

            if let Some(exp) = &expected_val {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let act: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
                    panic!("{ctx}: stdout is not valid JSON: {e}\nstdout:\n{stdout}")
                });
                assert_envelope_shape(&ctx, exp, &act);
            }
        }
    }
}

#[test]
fn readme_console_blocks_run_and_match_envelopes() {
    let readme = fs::read_to_string(repo_root().join("README.md")).expect("read README.md");
    let lab_blocks = extract_fenced_blocks(&readme, "console,lab");
    assert!(
        !lab_blocks.is_empty(),
        "README.md must tag network/GPU blocks with ```console,lab"
    );
    run_doc_file("README.md", "readme");
}

#[test]
fn user_sta_guide_console_blocks_run_and_match_envelopes() {
    run_doc_file("docs/user/sta/README.md", "user-sta");
}

#[test]
fn user_cli_guide_console_blocks_run_and_match_envelopes() {
    run_doc_file("docs/user/cli/README.md", "user-cli");
}
