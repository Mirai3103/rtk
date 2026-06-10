//! Filters `flutter pub get` / `flutter pub upgrade` output.
//!
//! Suppresses the per-package list and keeps only the summary lines,
//! preserving error output in full.

use crate::core::runner::{self, RunOptions};
use crate::core::utils::resolved_command;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    // Package change lines: "+ pkg ver", "- pkg ver", "> pkg v1 -> v2", "  pkg ver (X available)"
    static ref RE_PKG_LINE: Regex =
        Regex::new(r"^([+\->] |\s+\S+ \S+ \()").unwrap();
    // Lines to drop entirely (not errors, not summaries)
    static ref RE_NOISE: Regex =
        Regex::new(r"^(Downloading packages\.\.\.|Try `flutter pub outdated`|Try `dart pub outdated`)").unwrap();
}

pub fn run(subcommand: &str, args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("flutter");
    cmd.arg("pub").arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: flutter pub {} {}", subcommand, args.join(" "));
    }

    runner::run_filtered(
        cmd,
        &format!("flutter pub {}", subcommand),
        &args.join(" "),
        move |raw| filter_pub_output(raw, subcommand),
        RunOptions::default(),
    )
}

fn filter_pub_output(raw: &str, subcommand: &str) -> String {
    let mut added: usize = 0;
    let mut removed: usize = 0;
    let mut upgraded: usize = 0;
    let mut downgraded: usize = 0;
    let mut kept = Vec::new();

    for line in raw.lines() {
        if RE_NOISE.is_match(line) {
            continue;
        }
        if line.starts_with("+ ") {
            added += 1;
            continue;
        }
        if line.starts_with("- ") {
            removed += 1;
            continue;
        }
        if line.starts_with("> ") {
            upgraded += 1;
            continue;
        }
        if line.starts_with("< ") {
            downgraded += 1;
            continue;
        }
        // "  pkg version (X.Y available)" — unchanged with newer available, suppress
        if RE_PKG_LINE.is_match(line) {
            continue;
        }
        kept.push(line);
    }

    // Build compact package summary line
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("{} added", added));
    }
    if upgraded > 0 {
        parts.push(format!("{} upgraded", upgraded));
    }
    if downgraded > 0 {
        parts.push(format!("{} downgraded", downgraded));
    }
    if removed > 0 {
        parts.push(format!("{} removed", removed));
    }

    let mut out = String::new();
    if !parts.is_empty() {
        out.push_str(&format!(
            "flutter pub {}: {}\n",
            subcommand,
            parts.join(", ")
        ));
    }

    // Append kept lines (summary, errors, warnings)
    for line in &kept {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    // Fallback: if nothing was filtered (e.g. already up to date with no packages listed),
    // return raw output so the user sees the original message.
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
    fn test_pub_get_normal() {
        let raw = "Resolving dependencies...\nDownloading packages...\n\
            + analyzer 13.0.0\n+ args 2.7.0\n+ build 4.0.6\n\
              meta 1.18.0 (1.18.2 available)\n\
            Changed 3 dependencies!\n\
            1 packages have newer versions incompatible with dependency constraints.\n\
            Try `flutter pub outdated` for more information.";

        let out = filter_pub_output(raw, "get");
        assert!(out.contains("flutter pub get: 3 added"), "got: {out}");
        assert!(out.contains("Changed 3 dependencies!"), "got: {out}");
        assert!(out.contains("1 packages have newer versions"), "got: {out}");
        assert!(!out.contains("+ analyzer"), "pkg line leaked: {out}");
        assert!(!out.contains("Downloading packages"), "noise leaked: {out}");
        assert!(!out.contains("Try `flutter pub"), "noise leaked: {out}");
    }

    #[test]
    fn test_pub_get_no_changes() {
        let raw = "Resolving dependencies...\nNo dependencies changed.\n";
        let out = filter_pub_output(raw, "get");
        assert!(out.contains("No dependencies changed"), "got: {out}");
    }

    #[test]
    fn test_pub_upgrade_shows_upgraded() {
        let raw = "Resolving dependencies...\nDownloading packages...\n\
            > http 1.0.0 -> 1.2.0\n> provider 6.0.0 -> 6.1.0\n\
            - old_pkg 1.0.0\n\
            Changed 3 dependencies!\n";

        let out = filter_pub_output(raw, "upgrade");
        assert!(out.contains("flutter pub upgrade: 2 upgraded, 1 removed"), "got: {out}");
        assert!(!out.contains("> http"), "pkg line leaked: {out}");
    }

    #[test]
    fn test_pub_get_error_preserved() {
        let raw = "Resolving dependencies...\n\
            Because rtk_flutter requires sdk >=3.12.0 and current sdk is 3.11.5, \
            version solving failed.\n\
            Failed to update packages.";

        let out = filter_pub_output(raw, "get");
        assert!(out.contains("version solving failed"), "error lost: {out}");
        assert!(out.contains("Failed to update packages"), "error lost: {out}");
    }

    #[test]
    fn test_token_savings_real_fixture() {
        let raw = include_str!("../../../tests/fixtures/flutter/flutter_pub_get_raw.txt");
        let out = filter_pub_output(raw, "get");

        let raw_tokens = count_tokens(raw);
        let out_tokens = count_tokens(&out);
        let savings = 100.0 - (out_tokens as f64 / raw_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "Expected ≥60% savings, got {:.1}% (raw={raw_tokens}, filtered={out_tokens})\nOutput:\n{out}",
            savings
        );
    }

    #[test]
    fn test_real_fixture_output() {
        let raw = include_str!("../../../tests/fixtures/flutter/flutter_pub_get_raw.txt");
        let out = filter_pub_output(raw, "get");

        // Should have a summary line
        assert!(out.contains("flutter pub get:"), "missing summary: {out}");
        // Should keep the Changed line
        assert!(out.contains("Changed"), "missing Changed line: {out}");
        // Should not contain individual package lines
        assert!(!out.contains("\n+ "), "pkg lines leaked: {out}");
        assert!(!out.contains("Downloading packages"), "noise leaked: {out}");
    }
}
