// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tests of the command line of the dual-PDK suite. The shared modules test themselves
//! (`fetch.rs`, `manifest.rs`, `json.rs`), as do the runner and the scan.

use super::*;

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// `pdk fetch [--dir DIR] [--only NAME...] [--offline VENDOR_DIR]`: the subcommand is required,
// options take `--opt val` or `--opt=val`, and unknown arguments fail.
#[test]
fn fetch_arguments() {
    let a = |s: &[&str]| fetch_args(&strings(s));
    assert_eq!(a(&["fetch"]), Ok(FetchOptions::default()));
    let dir = Ok(FetchOptions {
        dir: Some("libs".into()),
        only: Vec::new(),
        offline: None,
    });
    assert_eq!(a(&["fetch", "--dir", "libs"]), dir);
    assert_eq!(a(&["fetch", "--dir=libs"]), dir);
    assert_eq!(
        a(&[
            "fetch",
            "--dir",
            "libs",
            "--only",
            "sky130hd_tt",
            "--only=gf180mcu7t_tt,gf180mcu9t_tt",
            "--offline",
            "/vendor"
        ]),
        Ok(FetchOptions {
            dir: Some("libs".into()),
            only: strings(&["sky130hd_tt", "gf180mcu7t_tt", "gf180mcu9t_tt"]),
            offline: Some("/vendor".into()),
        })
    );
    for bad in [
        &[][..],
        &["get"],
        &["fetch", "--dir"],
        &["fetch", "--dir", "--require"],
        &["fetch", "--dir="],
        &["fetch", "--only"],
        &["fetch", "--offline"],
        &["fetch", "libs"],
        &["fetch", "--require"],
    ] {
        assert!(a(bad).is_err(), "{bad:?}");
    }
}

// `verify pdk [--dir DIR] [--profile pr|full] [--require] [--gpu] [--timeout SECS]`, in any
// order; the time limit defaults to 600 s and must be a positive number of seconds.
#[test]
fn verify_arguments() {
    let a = |s: &[&str]| verify_args(&strings(s));
    assert_eq!(a(&[]), Ok(VerifyOptions::default()));
    assert_eq!(VerifyOptions::default().timeout, Duration::from_secs(600));
    assert_eq!(VerifyOptions::default().profile, Profile::Pr);
    assert_eq!(
        a(&[
            "--timeout",
            "30",
            "--require",
            "--gpu",
            "--profile=full",
            "--dir=/x"
        ]),
        Ok(VerifyOptions {
            dir: Some("/x".into()),
            profile: Profile::Full,
            require: true,
            gpu: true,
            timeout: Duration::from_secs(30),
        })
    );
    assert_eq!(
        a(&["--dir", "d", "--profile", "pr"]).map(|o| (o.dir, o.profile, o.require)),
        Ok((Some("d".into()), Profile::Pr, false))
    );
    for bad in [
        &["--timeout", "0"][..],
        &["--timeout", "x"],
        &["--timeout"],
        &["--profile", "fast"],
        &["--profile"],
        &["sta"],
        &["--dir"],
    ] {
        assert!(a(bad).is_err(), "{bad:?}");
    }
}
