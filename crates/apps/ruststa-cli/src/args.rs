// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Command-line argument parsing and pre-scanning for `ruststa` (`STA-REQ-0500` to `STA-REQ-0566`).

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Output format (`--format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Format {
    #[default]
    Text,
    Json,
}

impl Format {
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Color control (`--color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

/// Progress control (`--progress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Progress {
    #[default]
    Auto,
    Always,
    Never,
    Live,
    Log,
    Off,
}

/// Precision choice (`--precision`, `STA-REQ-0503`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PrecisionArg {
    #[default]
    F64,
    F32,
}

impl PrecisionArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::F64 => "f64",
            Self::F32 => "f32",
        }
    }
}

/// Storage mode (`--sta-store` / `--store`, `STA-REQ-0504`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum StoreArg {
    #[default]
    Full,
    Summary,
}

impl StoreArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Summary => "summary",
        }
    }
}

/// Verification method (`--method`, `STA-REQ-0520`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CheckMethod {
    #[default]
    Independent,
    Full,
    Both,
}

impl CheckMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::Full => "full",
            Self::Both => "both",
        }
    }
}

/// Path / report check kind (`--kind`, `STA-REQ-0532`, `STA-REQ-0552`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PathKind {
    Setup,
    Hold,
    #[default]
    Both,
}

/// Report section (`--section`, `STA-REQ-0551`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum ReportSection {
    Summary,
    Paths,
    Endpoints,
    Pins,
    Clocks,
    Checks,
    Unconstrained,
    Notes,
}

/// Pre-scanned global flags before full Clap parsing (`STA-REQ-0501`).
#[derive(Debug, Clone)]
pub struct PreScan {
    pub format: Format,
    pub invalid_format: Option<String>,
    pub workspace_raw: Option<String>,
    pub base_rev_raw: Option<String>,
    pub subcommand_hint: Option<String>,
    pub subcommand_name: Option<String>,
    pub quiet: bool,
}

/// Scans raw arguments before Clap so `--format json` is honoured even when Clap rejects arguments.
pub fn prescan(args: &[OsString]) -> PreScan {
    let mut format = Format::Text;
    let mut invalid_format = None;
    let mut workspace_raw = None;
    let mut base_rev_raw = None;
    let mut subcommand_hint = None;
    let mut subcommand_name = None;
    let mut quiet = false;

    let known_subcommands = [
        "run",
        "update",
        "check",
        "path",
        "inspect",
        "report",
        "whatif",
        "completions",
    ];
    let mut i = 1;
    while i < args.len() {
        let s = args[i].to_string_lossy();
        if s == "--" {
            break;
        }
        if s == "-q" || s == "--quiet" {
            quiet = true;
        } else if s == "--format" {
            if let Some(next) = args.get(i + 1) {
                let val = next.to_string_lossy();
                match Format::parse_str(&val) {
                    Some(f) => format = f,
                    None => invalid_format = Some(val.into_owned()),
                }
                i += 2;
                continue;
            }
        } else if let Some(val) = s.strip_prefix("--format=") {
            match Format::parse_str(val) {
                Some(f) => format = f,
                None => invalid_format = Some(val.to_string()),
            }
        } else if s == "-w" || s == "--workspace" {
            if let Some(next) = args.get(i + 1) {
                workspace_raw = Some(next.to_string_lossy().into_owned());
                i += 2;
                continue;
            }
        } else if let Some(val) = s.strip_prefix("--workspace=") {
            workspace_raw = Some(val.to_string());
        } else if let Some(val) = s.strip_prefix("-w=") {
            workspace_raw = Some(val.to_string());
        } else if s == "--base-rev" {
            if let Some(next) = args.get(i + 1) {
                base_rev_raw = Some(next.to_string_lossy().into_owned());
                i += 2;
                continue;
            }
        } else if let Some(val) = s.strip_prefix("--base-rev=") {
            base_rev_raw = Some(val.to_string());
        } else if matches!(
            s.as_ref(),
            "--threads"
                | "--color"
                | "--progress"
                | "--rev"
                | "--corner"
                | "--backend"
                | "--precision"
                | "--max-paths"
                | "--sta-store"
                | "--store"
                | "--tolerance"
                | "-m"
                | "--message"
                | "--method"
                | "--target"
                | "--to"
                | "--kind"
                | "--inst"
                | "--net"
                | "--pin"
                | "--section"
                | "--sort"
                | "--filter"
                | "--limit"
                | "--edits"
                | "--max-candidates"
                | "--max-edits"
                | "--max-state-gib"
                | "--timeout"
        ) {
            i += 2;
            continue;
        } else if !s.starts_with('-') && subcommand_hint.is_none() {
            if known_subcommands.contains(&s.as_ref()) {
                subcommand_name = Some(s.to_string());
                subcommand_hint = Some(format!("ruststa {s}"));
            } else {
                subcommand_hint = Some("ruststa".to_string());
            }
        }
        i += 1;
    }

    PreScan {
        format,
        invalid_format,
        workspace_raw,
        base_rev_raw,
        subcommand_hint,
        subcommand_name,
        quiet,
    }
}

/// Returns the Clap [`clap::Command`] definition for `ruststa`.
pub fn command() -> clap::Command {
    <Cli as clap::CommandFactory>::command()
}

/// Top-level `ruststa` CLI (`STA-REQ-0500`).
#[derive(Debug, Parser)]
#[command(
    name = "ruststa",
    about = "Static timing analysis on a RustEDA workspace",
    disable_version_flag = true
)]
pub struct Cli {
    /// Print version information and exit.
    #[arg(short = 'V', long = "version", global = true)]
    pub version: bool,

    /// Workspace directory (`-w, --workspace <DIR>`). Required except for `completions`.
    #[arg(short = 'w', long = "workspace", global = true, value_name = "DIR")]
    pub workspace: Option<PathBuf>,

    /// Output format (`text` or `json`).
    #[arg(long = "format", global = true, default_value_t = Format::Text, value_enum)]
    pub format: Format,

    /// Worker thread count (`0` = all available CPUs).
    #[arg(long = "threads", global = true, default_value_t = 0, value_name = "N")]
    pub threads: usize,

    /// Suppress note diagnostics on stderr.
    #[arg(short = 'q', long = "quiet", global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Emit additional progress diagnostics on stderr.
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Terminal color policy.
    #[arg(long = "color", global = true, default_value_t = Color::Auto, value_enum)]
    pub color: Color,

    /// Progress reporting policy.
    #[arg(long = "progress", global = true, default_value_t = Progress::Auto, value_enum)]
    pub progress: Progress,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Subcommands of `ruststa` (`STA-REQ-0500`).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run static timing analysis on a workspace revision and commit `sta.rbin`.
    Run(RunArgs),
    /// Incrementally update timing analysis from a base revision and commit `sta.rbin`.
    Update(UpdateArgs),
    /// Verify `sta.rbin` of a revision and commit `sta_check.json`.
    Check(CheckArgs),
    /// Query timing paths from `sta.rbin`.
    Path(PathArgs),
    /// Inspect instances, nets, pins, ports, paths, or selections.
    Inspect(InspectArgs),
    /// Render timing reports from `sta.rbin`.
    Report(ReportArgs),
    /// Evaluate candidate netlist edits on a revision, read-only (`rusteda.edits.v1`).
    Whatif(WhatifArgs),
    /// Print shell completion script to stdout.
    Completions(CompletionsArgs),
}

impl Command {
    pub fn envelope_command_name(&self) -> &'static str {
        match self {
            Self::Run(_) => "ruststa run",
            Self::Update(_) => "ruststa update",
            Self::Check(_) => "ruststa check",
            Self::Path(_) => "ruststa path",
            Self::Inspect(_) => "ruststa inspect",
            Self::Report(_) => "ruststa report",
            Self::Whatif(_) => "ruststa whatif",
            Self::Completions(_) => "ruststa completions",
        }
    }

    pub fn is_mutation(&self) -> bool {
        matches!(self, Self::Run(_) | Self::Update(_) | Self::Check(_))
    }
}

/// Arguments for `ruststa run` (`STA-REQ-0502` to `STA-REQ-0509`).
#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Base revision to analyze (`rev<N>`; `HEAD` is rejected on mutations).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,

    /// Corner name(s) to analyze (repeatable; defaults to all corners in the revision).
    #[arg(long = "corner", value_delimiter = ',', value_name = "NAME")]
    pub corners: Vec<String>,

    /// Propagation backend (`cpu`, `cuda`, `gpu`).
    #[arg(long = "backend", default_value = "cpu", value_name = "BACKEND")]
    pub backend: String,

    /// Floating-point precision (`f64` or `f32`).
    #[arg(long = "precision", default_value_t = PrecisionArg::F64, value_enum)]
    pub precision: PrecisionArg,

    /// Maximum number of ranked setup and hold paths to store per corner.
    #[arg(long = "max-paths", default_value_t = 5, value_name = "N")]
    pub max_paths: usize,

    /// Limit of the estimated propagation state of one corner, in GiB (`T0023` above it).
    #[arg(long = "max-state-gib", default_value_t = 16.0, value_name = "GIB")]
    pub max_state_gib: f64,

    /// Storage mode for `sta.rbin` (`full` or `summary`).
    #[arg(
        long = "sta-store",
        visible_alias = "store",
        default_value_t = StoreArg::Full,
        value_enum
    )]
    pub sta_store: StoreArg,

    /// Also run independent timing verification in the same revision.
    #[arg(long = "check")]
    pub check: bool,

    /// Tolerance in nanoseconds for `--check`.
    #[arg(long = "tolerance", value_name = "NS")]
    pub tolerance: Option<f64>,

    /// Commit message recorded in `meta.json` and `history.jsonl`.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
}

/// Arguments for `ruststa update` (`STA-REQ-0510` to `STA-REQ-0515`).
#[derive(Debug, Clone, Args)]
pub struct UpdateArgs {
    /// Base revision (`rev<N>`; `HEAD` is rejected on mutations).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,

    /// Corner name(s) to analyze.
    #[arg(long = "corner", value_delimiter = ',', value_name = "NAME")]
    pub corners: Vec<String>,

    /// Propagation backend (`cpu`, `cuda`, `gpu`).
    #[arg(long = "backend", default_value = "cpu", value_name = "BACKEND")]
    pub backend: String,

    /// Floating-point precision (`f64` or `f32`).
    #[arg(long = "precision", default_value_t = PrecisionArg::F64, value_enum)]
    pub precision: PrecisionArg,

    /// Maximum number of ranked setup and hold paths to store per corner.
    #[arg(long = "max-paths", default_value_t = 5, value_name = "N")]
    pub max_paths: usize,

    /// Limit of the estimated propagation state of one corner, in GiB (`T0023` above it).
    #[arg(long = "max-state-gib", default_value_t = 16.0, value_name = "GIB")]
    pub max_state_gib: f64,

    /// Storage mode for `sta.rbin` (`full` or `summary`).
    #[arg(
        long = "sta-store",
        visible_alias = "store",
        default_value_t = StoreArg::Full,
        value_enum
    )]
    pub sta_store: StoreArg,

    /// Also run independent timing verification in the same revision.
    #[arg(long = "check")]
    pub check: bool,

    /// Tolerance in nanoseconds for `--check`.
    #[arg(long = "tolerance", value_name = "NS")]
    pub tolerance: Option<f64>,

    /// Verify incremental update against a full recomputation.
    #[arg(long = "verify-full")]
    pub verify_full: bool,

    /// Commit message recorded in `meta.json` and `history.jsonl`.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
}

/// Arguments for `ruststa check` (`STA-REQ-0520` to `STA-REQ-0529`).
#[derive(Debug, Clone, Args)]
pub struct CheckArgs {
    /// Base revision whose `sta` table will be verified (`rev<N>`).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,

    /// Verification method (`independent`, `full`, `both`).
    #[arg(long = "method", default_value_t = CheckMethod::Independent, value_enum)]
    pub method: CheckMethod,

    /// Tolerance in nanoseconds (`> 0` and finite).
    #[arg(long = "tolerance", value_name = "NS")]
    pub tolerance: Option<f64>,

    /// Commit message recorded in `meta.json` and `history.jsonl`.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
}

/// Arguments for `ruststa path` (`STA-REQ-0530` to `STA-REQ-0539`).
#[derive(Debug, Clone, Args)]
pub struct PathArgs {
    /// Revision to query (`rev<N>` or `HEAD`).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,

    /// Handle target (`path<N>`, `pin<N>`, `port<N>`).
    #[arg(long = "target", value_delimiter = ',', value_name = "HANDLE")]
    pub targets: Vec<String>,

    /// Endpoint pin or port name (`--to <NAME>`).
    #[arg(long = "to", value_delimiter = ',', value_name = "NAME")]
    pub to: Vec<String>,

    /// Path check kind (`setup`, `hold`, `both`).
    #[arg(long = "kind", default_value_t = PathKind::Both, value_enum)]
    pub kind: PathKind,

    /// Corner to query (defaults to worst setup corner).
    #[arg(long = "corner", value_name = "NAME")]
    pub corner: Option<String>,

    /// Print full precision values without rounding in text mode.
    #[arg(long = "exact")]
    pub exact: bool,
}

/// Arguments for `ruststa inspect` (`STA-REQ-0540` to `STA-REQ-0549`).
#[derive(Debug, Clone, Args)]
pub struct InspectArgs {
    /// Revision to query (`rev<N>` or `HEAD`).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,

    /// Handle target (`sel<N>`, `inst<N>`, `net<N>`, `pin<N>`, `port<N>`, `path<N>`).
    #[arg(long = "target", value_delimiter = ',', value_name = "HANDLE")]
    pub targets: Vec<String>,

    /// Instance name (`--inst <NAME>`).
    #[arg(long = "inst", value_name = "NAME")]
    pub inst: Option<String>,

    /// Net name (`--net <NAME>`).
    #[arg(long = "net", value_name = "NAME")]
    pub net: Option<String>,

    /// Pin or port name (`--pin <NAME>`).
    #[arg(long = "pin", value_name = "NAME")]
    pub pin: Option<String>,

    /// Corner to inspect (defaults to worst setup corner).
    #[arg(long = "corner", value_name = "NAME")]
    pub corner: Option<String>,
}

/// Arguments for `ruststa report` (`STA-REQ-0550` to `STA-REQ-0560`).
#[derive(Debug, Clone, Args)]
pub struct ReportArgs {
    /// Revision to query (`rev<N>` or `HEAD`).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,

    /// Report sections to include (`summary`, `paths`, `endpoints`, `pins`, `clocks`, `checks`, `unconstrained`, `notes`).
    #[arg(
        long = "section",
        value_enum,
        value_delimiter = ',',
        value_name = "SECTION"
    )]
    pub sections: Vec<ReportSection>,

    /// Path check kind (`setup`, `hold`, `both`).
    #[arg(long = "kind", default_value_t = PathKind::Both, value_enum)]
    pub kind: PathKind,

    /// Corner to report (defaults to worst setup corner).
    #[arg(long = "corner", value_name = "NAME")]
    pub corner: Option<String>,

    /// Sort key for tabular sections (`slack`, `arrival`, `required`, `endpoint`, `pin`, `net`, `cap`, `slew`, `fanout`, `period`, `name`).
    #[arg(long = "sort", value_name = "KEY")]
    pub sort: Option<String>,

    /// Reverse the sort order.
    #[arg(long = "reverse")]
    pub reverse: bool,

    /// Filter expression (`slack<0`, `violating`, `clock=<NAME>`, `kind=setup|hold`, `endpoint~<SUBSTR>`, `pin~<SUBSTR>`).
    #[arg(long = "filter", value_name = "EXPR")]
    pub filter: Option<String>,

    /// Maximum rows per tabular section.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,

    /// Print full precision values without rounding in text mode.
    #[arg(long = "exact")]
    pub exact: bool,
}

/// Arguments for `ruststa whatif` (`STA-REQ-0620` to `STA-REQ-0628`).
#[derive(Debug, Clone, Args)]
pub struct WhatifArgs {
    /// Revision whose netlist the edits apply to (`rev<N>` or `HEAD`).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,

    /// Edit file in the `rusteda.edits.v1` format.
    #[arg(long = "edits", value_name = "FILE")]
    pub edits: PathBuf,

    /// Corner name(s) to analyze (defaults to all corners in the revision).
    #[arg(long = "corner", value_delimiter = ',', value_name = "NAME")]
    pub corners: Vec<String>,

    /// Maximum number of candidates in the edit file (at most 1024).
    #[arg(long = "max-candidates", default_value_t = 64, value_name = "N")]
    pub max_candidates: usize,

    /// Maximum number of base edits and of edits per candidate (at most 4096).
    #[arg(long = "max-edits", default_value_t = 256, value_name = "N")]
    pub max_edits: usize,

    /// Limit of the estimated state of the timing sessions, in GiB (`T0023` above it).
    #[arg(long = "max-state-gib", default_value_t = 16.0, value_name = "GIB")]
    pub max_state_gib: f64,

    /// Wall-clock budget in seconds; candidates beyond it are not evaluated (`T0108`, exit 5).
    #[arg(long = "timeout", value_name = "SECONDS")]
    pub timeout: Option<f64>,
}

/// Arguments for `ruststa completions` (`STA-REQ-0565`).
#[derive(Debug, Clone, Args)]
pub struct CompletionsArgs {
    /// Target shell (`bash`, `zsh`, `fish`, `elvish`, `powershell`). Inferred from `$SHELL` if omitted.
    #[arg(value_name = "SHELL")]
    pub shell: Option<String>,
}
