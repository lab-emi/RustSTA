// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `ruststa` command-line program on the RustEDA workspace (`docs/specs/cli/README.md`,
//! `docs/specs/sta/cli.md`, ADR 0008).

pub mod args;
pub mod check;
pub mod inspect;
pub mod path;
pub mod phases;
pub mod report;
pub mod run;
pub mod store_build;
pub mod update;
pub mod whatif;
pub mod ws_io;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use clap_complete::Shell;
use rusteda_workspace::text::render_diagnostic;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, ErrorCode, Invocation, JsonStyle, Rev, Severity,
    Workspace,
};

use crate::args::{Cli, Color, Command, Format, PreScan, command, prescan};

/// Program version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Application diagnostic codes (`CLI-REQ-0006`, `WS-REQ-0097`).
pub const CODE_THREAD_POOL: DiagCode = DiagCode::of("A0001");
pub const CODE_INTERNAL: DiagCode = DiagCode::of("A0002");
pub const CODE_USAGE: DiagCode = DiagCode::of("A0003");
pub const CODE_SHELL: DiagCode = DiagCode::of("A0030");
pub const CODE_CANCELLED: DiagCode = DiagCode::of("A0130");
pub const CODE_BACKEND: DiagCode = DiagCode::of("A0501");

/// Compiled propagation backends of this build (`CLI-REQ-0001`).
pub const fn backends() -> &'static [&'static str] {
    #[cfg(feature = "cuda")]
    {
        &["cpu", "cuda"]
    }
    #[cfg(not(feature = "cuda"))]
    {
        &["cpu"]
    }
}

fn app_diag_code(code: ErrorCode) -> DiagCode {
    match code {
        ErrorCode::NotAWorkspace
        | ErrorCode::WorkspaceExists
        | ErrorCode::UnknownRevision
        | ErrorCode::RevisionConflict
        | ErrorCode::MissingTable
        | ErrorCode::CorruptTable
        | ErrorCode::SchemaTooNew => DiagCode::of("T0001"),
        ErrorCode::UnknownHandle => DiagCode::of("T0008"),
        ErrorCode::Usage => CODE_USAGE,
        ErrorCode::InputRejected => DiagCode::of("T0001"),
        ErrorCode::BackendUnavailable => CODE_BACKEND,
        ErrorCode::IoError => DiagCode::of("A0004"),
        ErrorCode::Internal => CODE_INTERNAL,
        ErrorCode::Cancelled => CODE_CANCELLED,
    }
}

fn should_color_stderr(color: Color) -> bool {
    match color {
        Color::Always => true,
        Color::Never => false,
        Color::Auto => {
            if !io::stderr().is_terminal() {
                return false;
            }
            if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                return false;
            }
            if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
                return false;
            }
            true
        }
    }
}

fn format_diag_line(diag: &Diagnostic, color: bool) -> String {
    let rendered = render_diagnostic(diag);
    if !color {
        return rendered;
    }
    let (word, ansi) = match diag.severity {
        Severity::Error => ("error", "\x1b[1;31merror\x1b[0m"),
        Severity::Warning => ("warning", "\x1b[1;33mwarning\x1b[0m"),
        Severity::Note => ("note", "\x1b[1;36mnote\x1b[0m"),
    };
    if let Some(rest) = rendered.strip_prefix(word) {
        format!("{ansi}{rest}")
    } else {
        rendered
    }
}

fn write_stderr_diags(diags: &[Diagnostic], quiet: bool, color: Color, stderr: &mut dyn Write) {
    let use_color = should_color_stderr(color);
    for d in diags {
        if quiet && d.severity == Severity::Note {
            continue;
        }
        let _ = writeln!(stderr, "{}", format_diag_line(d, use_color));
    }
}

fn write_stdout_str(text: &str, stdout: &mut dyn Write) {
    // CLI-REQ-0011: broken pipe on standard output ends output silently.
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

#[allow(clippy::too_many_arguments)]
fn finish_error(
    cmd_name: &str,
    ws_arg: Option<&str>,
    base_rev: Option<Rev>,
    err: Error,
    mut diags: Vec<Diagnostic>,
    format: Format,
    quiet: bool,
    color: Color,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let code = err.exit_code().clamp(1, 255) as u8;
    if !diags.iter().any(|d| d.severity == Severity::Error) {
        let mut d = err.to_diagnostic(app_diag_code(err.code()));
        if let Error::UnknownHandle { handle, .. } = &err
            && d.help.is_none()
            && let Some(sugg) = ws_io::strip_underscore_handle_suggestion(handle)
        {
            d = d.with_help(format!("did you mean '{sugg}'?"));
        }
        diags.push(d);
    }

    let mut env = Envelope::failure(cmd_name, &err);
    env.workspace = ws_arg.map(String::from);
    env.base_rev = base_rev;
    env.diagnostics = diags;
    if let (Error::RevisionConflict { base, head }, Some(w)) = (&err, ws_arg) {
        let base_s = base.to_string();
        let head_s = head.to_string();
        env.push_next_action(&["rusteda", "log", "-w", w]);
        env.push_next_action(&[
            "rusteda", "diff", "-w", w, "--from", &base_s, "--to", &head_s,
        ]);
        env.push_next_action(&["rusteda", "show", "-w", w, "--rev", &head_s]);
    }
    if let (Error::MissingTable { rev, table }, Some(w)) = (&err, ws_arg)
        && table == "sta"
    {
        let rev_s = rev.to_string();
        env.push_next_action(&["ruststa", "run", "-w", w, "--base-rev", &rev_s]);
    }

    match format {
        Format::Json => {
            let mut text = env.to_json(JsonStyle::Pretty);
            text.push('\n');
            write_stdout_str(&text, stdout);
        }
        Format::Text => {
            write_stderr_diags(&env.diagnostics, quiet, color, stderr);
        }
    }
    ExitCode::from(code)
}

#[allow(clippy::too_many_arguments)]
fn finish_ok(
    env: Envelope,
    text_lines: Vec<String>,
    exit_code: i32,
    format: Format,
    quiet: bool,
    color: Color,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    match format {
        Format::Json => {
            let mut text = env.to_json(JsonStyle::Pretty);
            text.push('\n');
            write_stdout_str(&text, stdout);
        }
        Format::Text => {
            write_stderr_diags(&env.diagnostics, quiet, color, stderr);
            if !text_lines.is_empty() {
                let mut body = text_lines.join("\n");
                body.push('\n');
                write_stdout_str(&body, stdout);
            }
        }
    }
    ExitCode::from(exit_code.clamp(0, 255) as u8)
}

fn maybe_log_usage_rejection(pre: &PreScan, argv_strings: &[String], err: &Error) {
    let Some(sub) = pre.subcommand_name.as_deref() else {
        return;
    };
    if !matches!(sub, "run" | "update" | "check") {
        return;
    }
    let Some(ws_dir) = pre.workspace_raw.as_deref() else {
        return;
    };
    if let Ok(ws) = Workspace::open(ws_dir) {
        let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
        let inv = Invocation::new(
            "ruststa",
            VERSION,
            format!("ruststa {sub}"),
            argv_strings.to_vec(),
        );
        let _ = ws.log_rejected(&inv, base_rev, err);
    }
}

fn resolve_shell(shell_arg: Option<&str>) -> Result<Shell, String> {
    let raw = if let Some(s) = shell_arg {
        s.to_string()
    } else if let Ok(env_shell) = std::env::var("SHELL") {
        std::path::Path::new(&env_shell)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(env_shell)
    } else {
        return Err(
            "cannot detect shell from environment; pass one of: bash, elvish, fish, powershell, zsh"
                .to_string(),
        );
    };

    match raw.to_ascii_lowercase().as_str() {
        "bash" => Ok(Shell::Bash),
        "elvish" => Ok(Shell::Elvish),
        "fish" => Ok(Shell::Fish),
        "powershell" | "pwsh" => Ok(Shell::PowerShell),
        "zsh" => Ok(Shell::Zsh),
        other => Err(format!(
            "unsupported shell '{other}': expected one of bash, elvish, fish, powershell, zsh"
        )),
    }
}

fn emit_version(
    cmd_name: &str,
    ws_arg: Option<String>,
    format: Format,
    quiet: bool,
    color: Color,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let version_line = format!("ruststa {VERSION} (backends: {})", backends().join(", "));
    if format == Format::Json {
        let mut env = Envelope::new(cmd_name);
        env.workspace = ws_arg;
        let _ = env.set("version", VERSION);
        let _ = env.set("backends", backends());
        return finish_ok(
            env,
            Vec::new(),
            0,
            Format::Json,
            quiet,
            color,
            stdout,
            stderr,
        );
    }
    write_stdout_str(&format!("{version_line}\n"), stdout);
    ExitCode::from(0)
}

/// Runs `ruststa` with `args` (including `argv[0]`) writing to standard output and standard error.
pub fn run(args: Vec<OsString>) -> ExitCode {
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    run_with_io(args, &mut stdout, &mut stderr)
}

/// Runs `ruststa` with custom output streams (`stdout` and `stderr`).
pub fn run_with_io<I, T>(args: I, stdout: &mut dyn Write, stderr: &mut dyn Write) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args_vec: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let pre = prescan(&args_vec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_inner(args_vec, &pre, stdout, stderr)
    }));
    match result {
        Ok(code) => code,
        Err(payload) => {
            let panic_msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unexpected panic".to_string()
            };
            let cmd_name = pre
                .subcommand_hint
                .as_deref()
                .unwrap_or("ruststa")
                .to_string();
            let err = Error::internal(format!("internal error: {panic_msg}"));
            let diag = Diagnostic::new(
                Severity::Error,
                CODE_INTERNAL,
                format!("internal error: {panic_msg}"),
            );
            let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
            finish_error(
                &cmd_name,
                pre.workspace_raw.as_deref(),
                base_rev,
                err,
                vec![diag],
                pre.format,
                pre.quiet,
                Color::Auto,
                stdout,
                stderr,
            )
        }
    }
}

fn run_inner(
    args: Vec<OsString>,
    pre: &PreScan,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let argv_strings: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cmd_name = pre
        .subcommand_hint
        .as_deref()
        .unwrap_or("ruststa")
        .to_string();

    if let Some(bad_fmt) = &pre.invalid_format {
        let msg =
            format!("invalid value '{bad_fmt}' for '--format <FORMAT>': expected 'text' or 'json'");
        let err = Error::usage(&msg);
        maybe_log_usage_rejection(pre, &argv_strings, &err);
        let diag = Diagnostic::new(Severity::Error, CODE_USAGE, msg)
            .with_help(format!("{cmd_name} --help lists every option"));
        let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
        return finish_error(
            &cmd_name,
            pre.workspace_raw.as_deref(),
            base_rev,
            err,
            vec![diag],
            pre.format,
            pre.quiet,
            Color::Auto,
            stdout,
            stderr,
        );
    }

    let matches = match command().try_get_matches_from(&args) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind;
            match e.kind() {
                ErrorKind::DisplayVersion => {
                    return emit_version(
                        &cmd_name,
                        pre.workspace_raw.clone(),
                        pre.format,
                        pre.quiet,
                        Color::Auto,
                        stdout,
                        stderr,
                    );
                }
                ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    let help_text = e.render().to_string();
                    if pre.format == Format::Json {
                        let mut env = Envelope::new(&cmd_name);
                        env.workspace = pre.workspace_raw.clone();
                        let _ = env.set("help", help_text.trim_end());
                        return finish_ok(
                            env,
                            Vec::new(),
                            0,
                            Format::Json,
                            pre.quiet,
                            Color::Auto,
                            stdout,
                            stderr,
                        );
                    }
                    write_stdout_str(&help_text, stdout);
                    return ExitCode::from(0);
                }
                _ => {
                    let rendered = e.render().to_string();
                    let first = rendered
                        .lines()
                        .next()
                        .unwrap_or("invalid command line")
                        .trim();
                    let msg = first.strip_prefix("error: ").unwrap_or(first).to_string();
                    let err = Error::usage(&msg);
                    maybe_log_usage_rejection(pre, &argv_strings, &err);
                    let diag = Diagnostic::new(Severity::Error, CODE_USAGE, msg)
                        .with_help(format!("{cmd_name} --help lists every option"));
                    let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
                    return finish_error(
                        &cmd_name,
                        pre.workspace_raw.as_deref(),
                        base_rev,
                        err,
                        vec![diag],
                        pre.format,
                        pre.quiet,
                        Color::Auto,
                        stdout,
                        stderr,
                    );
                }
            }
        }
    };

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            let err = Error::usage(&msg);
            maybe_log_usage_rejection(pre, &argv_strings, &err);
            let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
            return finish_error(
                &cmd_name,
                pre.workspace_raw.as_deref(),
                base_rev,
                err,
                Vec::new(),
                pre.format,
                pre.quiet,
                Color::Auto,
                stdout,
                stderr,
            );
        }
    };

    let format = cli.format;
    let quiet = cli.quiet;
    let color = cli.color;

    if cli.version {
        return emit_version(
            &cmd_name,
            cli.workspace.as_ref().map(|p| p.display().to_string()),
            format,
            quiet,
            color,
            stdout,
            stderr,
        );
    }

    let Some(subcmd) = &cli.command else {
        let mut cmd_def = Cli::command();
        let help_text = cmd_def.render_help().to_string();
        if format == Format::Json {
            let mut env = Envelope::new("ruststa");
            let _ = env.set("help", help_text.trim_end());
            return finish_ok(
                env,
                Vec::new(),
                0,
                Format::Json,
                quiet,
                color,
                stdout,
                stderr,
            );
        }
        write_stdout_str(&format!("{help_text}\n"), stdout);
        return ExitCode::from(0);
    };

    if let Command::Completions(comp) = subcmd {
        let shell = match resolve_shell(comp.shell.as_deref()) {
            Ok(s) => s,
            Err(msg) => {
                let diag = Diagnostic::new(Severity::Error, CODE_SHELL, msg.clone());
                let err = Error::usage(msg);
                return finish_error(
                    "ruststa completions",
                    None,
                    None,
                    err,
                    vec![diag],
                    format,
                    quiet,
                    color,
                    stdout,
                    stderr,
                );
            }
        };
        let mut buf = Vec::new();
        let mut cmd_def = command();
        clap_complete::generate(shell, &mut cmd_def, "ruststa", &mut buf);
        let script = String::from_utf8_lossy(&buf).into_owned();
        if format == Format::Json {
            let mut env = Envelope::new("ruststa completions");
            let _ = env.set("shell", &shell.to_string());
            let _ = env.set("script", &script);
            return finish_ok(
                env,
                Vec::new(),
                0,
                Format::Json,
                quiet,
                color,
                stdout,
                stderr,
            );
        }
        write_stdout_str(&script, stdout);
        return ExitCode::from(0);
    }

    let sub_name = match subcmd {
        Command::Run(_) => "ruststa run",
        Command::Update(_) => "ruststa update",
        Command::Check(_) => "ruststa check",
        Command::Path(_) => "ruststa path",
        Command::Inspect(_) => "ruststa inspect",
        Command::Report(_) => "ruststa report",
        Command::Whatif(_) => "ruststa whatif",
        Command::Completions(_) => "ruststa completions",
    };

    let custom_pool = if cli.threads > 0 {
        match rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build()
        {
            Ok(pool) => {
                let _ = rayon::ThreadPoolBuilder::new()
                    .num_threads(cli.threads)
                    .build_global();
                Some(pool)
            }
            Err(e) => {
                let msg = format!("failed to initialize worker thread pool: {e}");
                let err = Error::internal(&msg);
                let diag = Diagnostic::new(Severity::Error, CODE_THREAD_POOL, msg);
                let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
                return finish_error(
                    sub_name,
                    pre.workspace_raw.as_deref(),
                    base_rev,
                    err,
                    vec![diag],
                    format,
                    quiet,
                    color,
                    stdout,
                    stderr,
                );
            }
        }
    } else {
        None
    };

    let Some(ws_path) = cli.workspace.as_ref() else {
        let msg = format!("{sub_name} requires -w/--workspace <DIR>");
        let err = Error::usage(&msg);
        let diag = Diagnostic::new(Severity::Error, CODE_USAGE, msg)
            .with_help(format!("{sub_name} --help lists every option"));
        let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
        return finish_error(
            sub_name,
            None,
            base_rev,
            err,
            vec![diag],
            format,
            quiet,
            color,
            stdout,
            stderr,
        );
    };
    let ws_arg = ws_path.display().to_string();

    let ws = match Workspace::open(ws_path) {
        Ok(w) => w,
        Err(e) => {
            let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
            return finish_error(
                sub_name,
                Some(&ws_arg),
                base_rev,
                e,
                Vec::new(),
                format,
                quiet,
                color,
                stdout,
                stderr,
            );
        }
    };

    let inv = Invocation::new("ruststa", VERSION, sub_name, argv_strings);
    let mut diags = Vec::new();

    let mut exec_fn = || match subcmd {
        Command::Run(r) => run::exec_run(&ws, &ws_arg, r, &inv, &mut diags),
        Command::Update(u) => update::exec_update(&ws, &ws_arg, u, &inv, &mut diags),
        Command::Check(c) => check::exec_check(&ws, &ws_arg, c, &inv, &mut diags),
        Command::Path(p) => path::exec_path(&ws, &ws_arg, p, &mut diags),
        Command::Inspect(ins) => inspect::exec_inspect(&ws, &ws_arg, ins, &mut diags),
        Command::Report(rep) => report::exec_report(&ws, &ws_arg, rep, &mut diags),
        Command::Whatif(w) => whatif::exec_whatif(&ws, &ws_arg, w, &mut diags),
        Command::Completions(_) => unreachable!(),
    };

    let res = match &custom_pool {
        Some(pool) => pool.install(exec_fn),
        None => exec_fn(),
    };

    match res {
        Ok((mut env, lines, exit_code)) => {
            // The diagnostics of a successful command (warnings and notes of the analysis, of
            // the constraints and of the commit) belong to its envelope and its standard error,
            // like those of a failed one (AGENTS rule 7.1).
            diags.append(&mut env.diagnostics);
            env.diagnostics = diags;
            finish_ok(env, lines, exit_code, format, quiet, color, stdout, stderr)
        }
        Err(e) => {
            let base_rev = pre.base_rev_raw.as_deref().and_then(|s| Rev::parse(s).ok());
            finish_error(
                sub_name,
                Some(&ws_arg),
                base_rev,
                e,
                diags,
                format,
                quiet,
                color,
                stdout,
                stderr,
            )
        }
    }
}
