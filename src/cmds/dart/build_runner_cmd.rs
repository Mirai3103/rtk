//! Filters `dart run build_runner build/clean` output.
//!
//! Strips timestamp-prefixed progress spam; keeps warnings (W prefix) and the
//! final summary line. `watch` is passed through unchanged (streaming).

use crate::core::runner::{self, RunOptions};
use crate::core::utils::resolved_command;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::ffi::OsString;

lazy_static! {
    // "  7s json_serializable on ..." — timestamp progress lines
    static ref RE_PROGRESS: Regex = Regex::new(r"^\s+\d+s ").unwrap();
    // "  Built with build_runner/aot in 29s; wrote 3 outputs."
    static ref RE_SUMMARY: Regex =
        Regex::new(r"^\s+Built with .+ in (\d+)s; wrote (\d+) output").unwrap();
    // "W some warning message"
    static ref RE_WARNING: Regex = Regex::new(r"^W ").unwrap();
}

pub fn run(subcommand: &str, args: &[String], verbose: u8) -> Result<i32> {
    if subcommand == "watch" {
        let mut dart_args = vec![
            OsString::from("run"),
            OsString::from("build_runner"),
            OsString::from("watch"),
        ];
        dart_args.extend(args.iter().map(OsString::from));
        return crate::core::runner::run_passthrough("dart", &dart_args, verbose);
    }

    let mut cmd = resolved_command("dart");
    cmd.arg("run").arg("build_runner").arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: dart run build_runner {} {}", subcommand, args.join(" "));
    }

    let sub = subcommand.to_string();
    runner::run_filtered(
        cmd,
        &format!("dart run build_runner {}", subcommand),
        &args.join(" "),
        move |raw| filter_build_runner(raw, &sub),
        RunOptions::default(),
    )
}

fn filter_build_runner(raw: &str, subcommand: &str) -> String {
    let mut warnings: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut summary: Option<String> = None;
    let mut in_warning_body = false;

    for line in raw.lines() {
        if RE_WARNING.is_match(line) {
            in_warning_body = true;
            // Strip the "W " prefix and emit as [W] tag
            let msg = line.trim_start_matches("W ").trim();
            warnings.push(format!("[W] {}", msg));
            continue;
        }
        if in_warning_body && line.starts_with("  ") {
            warnings.push(line.to_string());
            continue;
        }
        in_warning_body = false;

        if RE_PROGRESS.is_match(line) {
            continue;
        }
        if let Some(caps) = RE_SUMMARY.captures(line) {
            let secs = caps.get(1).map_or("?", |m| m.as_str());
            let count = caps.get(2).map_or("?", |m| m.as_str());
            summary = Some(format!(
                "build_runner {}: done in {}s, wrote {} outputs",
                subcommand, secs, count
            ));
            continue;
        }
        // Keep errors and any other non-progress lines
        if !line.trim().is_empty() {
            kept.push(line.to_string());
        }
    }

    let mut out = String::new();
    if let Some(s) = summary {
        out.push_str(&s);
        out.push('\n');
    }
    if !warnings.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        for w in &warnings {
            out.push_str(w);
            out.push('\n');
        }
    }
    for line in &kept {
        out.push_str(line);
        out.push('\n');
    }

    if out.trim().is_empty() {
        return raw.to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn test_strips_progress() {
        let raw = "  1s compiling builders/aot\n  2s compiling builders/aot\n  \
                   3s json_serializable on 8 inputs; lib/main.dart\n  \
                   Built with build_runner/aot in 3s; wrote 0 outputs.\n";
        let out = filter_build_runner(raw, "build");
        assert!(!out.contains("compiling builders"), "progress leaked: {out}");
        assert!(!out.contains("json_serializable on 8"), "progress leaked: {out}");
        assert!(out.contains("build_runner build: done in 3s"), "no summary: {out}");
    }

    #[test]
    fn test_keeps_warnings() {
        let raw = "  1s compiling builders/aot\n\
                   W json_serializable on lib/models/product.dart:\n\
                     The version constraint is not allowed.\n\
                     Built with build_runner/aot in 5s; wrote 1 outputs.\n";
        let out = filter_build_runner(raw, "build");
        assert!(out.contains("[W] json_serializable on lib/models/product.dart"), "warning missing: {out}");
        assert!(out.contains("The version constraint is not allowed"), "warning body missing: {out}");
        assert!(!out.contains("1s compiling"), "progress leaked: {out}");
    }

    #[test]
    fn test_summary_format() {
        let raw = "  5s compiling builders/aot\n  \
                   Built with build_runner/aot in 29s; wrote 3 outputs.\n";
        let out = filter_build_runner(raw, "build");
        assert_eq!(out.trim(), "build_runner build: done in 29s, wrote 3 outputs");
    }

    #[test]
    fn test_token_savings_real_fixture() {
        let raw = include_str!("../../../tests/fixtures/dart/build_runner_build_raw.txt");
        let out = filter_build_runner(raw, "build");
        let raw_t = count_tokens(raw);
        let out_t = count_tokens(&out);
        let savings = 100.0 - (out_t as f64 / raw_t as f64 * 100.0);
        // Threshold reflects small fixture (1 warning + summary); real large builds save 85%+
        assert!(
            savings >= 50.0,
            "Expected ≥50% savings, got {:.1}% (raw={raw_t}, filtered={out_t})\nOutput:\n{out}",
            savings
        );
    }
}
