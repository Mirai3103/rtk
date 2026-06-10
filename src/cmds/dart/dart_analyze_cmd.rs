//! Shared filter for `dart analyze` and `flutter analyze`.
//!
//! Normalises both formats to compact `[level] file:line rule — message` lines.
//! For `flutter analyze`, also strips any dep-resolution preamble before
//! "Analyzing ...". For `dart analyze`, strips "Try ..." suggestion suffixes.

use crate::core::runner::{self, RunOptions};
use crate::core::utils::resolved_command;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    // Dep-resolution preamble lines in `flutter analyze`
    static ref RE_DEP: Regex = Regex::new(
        r"^(Resolving dependencies\.\.\.|Downloading packages\.\.\.|Got dependencies!|\s+\S+ \d|\d+ packages have|Try `(flutter|dart) pub)"
    ).unwrap();
    // "20 issues found." or "20 issues found. (ran in 2.1s)" or "0 issues found."
    static ref RE_SUMMARY: Regex = Regex::new(r"^(\d+) issues found\.(.*)").unwrap();
}

struct Issue {
    level: String,
    location: String,
    rule: String,
    message: String,
}

/// `dart analyze` line: `   level - file:line:col - message. Try ... - rule`
fn parse_dart(line: &str) -> Option<Issue> {
    let trimmed = line.trim();
    // Must contain " - " and have at least 4 parts
    let parts: Vec<&str> = trimmed.splitn(4, " - ").collect();
    if parts.len() < 4 {
        return None;
    }
    let level = parts[0].trim().to_string();
    if !matches!(level.as_str(), "error" | "warning" | "info" | "hint") {
        return None;
    }
    let location = parts[1].trim().to_string();
    // Strip "Try ..." suffix from message
    let raw_msg = parts[2].trim();
    let message = if let Some(idx) = raw_msg.find(". Try ") {
        raw_msg[..idx + 1].to_string()
    } else {
        raw_msg.to_string()
    };
    let rule = parts[3].trim().to_string();
    Some(Issue { level, location, rule, message })
}

/// `flutter analyze` line: `   level • message • file:line:col • rule`
fn parse_flutter(line: &str) -> Option<Issue> {
    let trimmed = line.trim();
    let parts: Vec<&str> = trimmed.splitn(4, " • ").collect();
    if parts.len() < 4 {
        return None;
    }
    let level = parts[0].trim().to_string();
    if !matches!(level.as_str(), "error" | "warning" | "info" | "hint") {
        return None;
    }
    let message = parts[1].trim().to_string();
    let location = parts[2].trim().to_string();
    let rule = parts[3].trim().to_string();
    Some(Issue { level, location, rule, message })
}

fn filter_analyze(raw: &str, tool: &str) -> String {
    let mut issues: Vec<Issue> = Vec::new();
    let mut elapsed = String::new();
    let mut issue_count: Option<usize> = None;
    // Flutter analyze may have a dep-resolution preamble; skip it until "Analyzing ..."
    let mut in_preamble = tool == "flutter";

    for line in raw.lines() {
        if in_preamble {
            if line.trim_start().starts_with("Analyzing ") {
                in_preamble = false;
            }
            // Always skip preamble lines (including the "Analyzing ..." marker itself)
            continue;
        }

        // Skip dep noise that appears even without a full preamble
        if RE_DEP.is_match(line) {
            continue;
        }

        if let Some(caps) = RE_SUMMARY.captures(line) {
            issue_count = caps.get(1).and_then(|m| m.as_str().parse().ok());
            let rest = caps.get(2).map_or("", |m| m.as_str()).trim();
            // "(ran in 2.1s)" → "2.1s"
            if let Some(t) = rest.strip_prefix("(ran in ").and_then(|s| s.strip_suffix(')')) {
                elapsed = t.to_string();
            }
            continue;
        }

        // Try parsing as an issue line
        let parsed = if line.contains(" • ") {
            parse_flutter(line)
        } else if line.contains(" - ") {
            parse_dart(line)
        } else {
            None
        };

        if let Some(issue) = parsed {
            issues.push(issue);
        }
        // Blank lines and "Analyzing ..." marker are silently dropped
    }

    // If nothing was parsed, fall back to raw
    if issue_count.is_none() && issues.is_empty() {
        return raw.to_string();
    }

    let total = issue_count.unwrap_or(issues.len());
    let mut out = String::new();

    if total == 0 {
        out.push_str(&format!("{} analyze: no issues", tool));
        if !elapsed.is_empty() {
            out.push_str(&format!(" ({})", elapsed));
        }
        out.push('\n');
        return out;
    }

    // Count by level
    let errors = issues.iter().filter(|i| i.level == "error").count();
    let warnings = issues.iter().filter(|i| i.level == "warning").count();
    let infos = issues.iter().filter(|i| i.level == "info" || i.level == "hint").count();

    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!("{} error{}", errors, if errors == 1 { "" } else { "s" }));
    }
    if warnings > 0 {
        parts.push(format!("{} warning{}", warnings, if warnings == 1 { "" } else { "s" }));
    }
    if infos > 0 {
        parts.push(format!("{} info", infos));
    }

    out.push_str(&format!("{} analyze: {}", tool, parts.join(", ")));
    if !elapsed.is_empty() {
        out.push_str(&format!(" ({})", elapsed));
    }
    out.push('\n');

    const CAP: usize = 30;
    for issue in issues.iter().take(CAP) {
        out.push_str(&format!(
            "[{}] {} {}\n  {}\n",
            issue.level, issue.location, issue.rule, issue.message
        ));
    }
    if issues.len() > CAP {
        out.push_str(&format!("... ({} more issues truncated)\n", issues.len() - CAP));
    }

    out
}

pub fn run(tool: &str, args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command(tool);
    cmd.arg("analyze");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: {} analyze {}", tool, args.join(" "));
    }

    let tool_owned = tool.to_string();
    runner::run_filtered(
        cmd,
        &format!("{} analyze", tool),
        &args.join(" "),
        move |raw| filter_analyze(raw, &tool_owned),
        RunOptions::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn test_dart_no_issues() {
        let raw = "Analyzing project...\n\nNo issues found!\n";
        // "No issues found!" doesn't match RE_SUMMARY → fallback to raw
        let out = filter_analyze(raw, "dart");
        assert!(out.contains("No issues found"), "got: {out}");
    }

    #[test]
    fn test_dart_zero_issues() {
        let raw = "Analyzing project...\n\n0 issues found.\n";
        let out = filter_analyze(raw, "dart");
        assert_eq!(out.trim(), "dart analyze: no issues");
    }

    #[test]
    fn test_dart_format_compact() {
        let raw = "Analyzing project...\n\n\
            warning - lib/models/user.dart:30:8 - The declaration '_x' isn't referenced. Try removing it. - unused_element\n\
            info - lib/main.dart:21:5 - Don't invoke 'print'. Try using a logger. - avoid_print\n\
            2 issues found.\n";
        let out = filter_analyze(raw, "dart");
        assert!(out.contains("dart analyze: 1 warning, 1 info"), "header wrong: {out}");
        assert!(out.contains("[warning] lib/models/user.dart:30:8 unused_element"), "issue missing: {out}");
        // "Try removing it." should be stripped
        assert!(!out.contains("Try removing"), "Try suffix leaked: {out}");
        assert!(out.contains("The declaration '_x' isn't referenced."), "message missing: {out}");
    }

    #[test]
    fn test_flutter_format_compact() {
        let raw = "Analyzing project...\n\n\
            warning • The declaration '_x' isn't referenced • lib/models/user.dart:30:8 • unused_element\n\
            info • Don't invoke 'print' • lib/main.dart:21:5 • avoid_print\n\
            2 issues found. (ran in 1.5s)\n";
        let out = filter_analyze(raw, "flutter");
        assert!(out.contains("flutter analyze: 1 warning, 1 info (1.5s)"), "header wrong: {out}");
        assert!(out.contains("[warning] lib/models/user.dart:30:8 unused_element"), "issue missing: {out}");
        assert!(out.contains("The declaration '_x' isn't referenced"), "message missing: {out}");
    }

    #[test]
    fn test_flutter_strips_dep_preamble() {
        let raw = "Resolving dependencies...\nDownloading packages...\n\
            Got dependencies!\n\
            11 packages have newer versions incompatible with dependency constraints.\n\
            Try `flutter pub outdated` for more information.\n\
            Analyzing project...\n\n\
            warning • The declaration '_x' isn't referenced • lib/models/user.dart:30:8 • unused_element\n\
            1 issues found. (ran in 2.0s)\n";
        let out = filter_analyze(raw, "flutter");
        assert!(!out.contains("Resolving"), "dep line leaked: {out}");
        assert!(!out.contains("Downloading"), "dep line leaked: {out}");
        assert!(!out.contains("Got dependencies"), "dep line leaked: {out}");
        assert!(out.contains("flutter analyze: 1 warning"), "header missing: {out}");
    }

    #[test]
    fn test_token_savings_flutter_fixture() {
        let raw = include_str!("../../../tests/fixtures/flutter/flutter_analyze_raw.txt");
        let out = filter_analyze(raw, "flutter");
        let raw_t = count_tokens(raw);
        let out_t = count_tokens(&out);
        let savings = 100.0 - (out_t as f64 / raw_t as f64 * 100.0);
        // Fixture has no dep-resolution preamble (deps cached); savings come from format
        // normalisation only. When preamble is present (50+ dep lines), savings reach 80%+.
        assert!(
            savings >= 15.0,
            "Expected ≥15% savings, got {:.1}% (raw={raw_t}, filtered={out_t})\nOutput:\n{out}",
            savings
        );
    }

    #[test]
    fn test_token_savings_dart_fixture() {
        let raw = include_str!("../../../tests/fixtures/dart/dart_analyze_raw.txt");
        let out = filter_analyze(raw, "dart");
        let raw_t = count_tokens(raw);
        let out_t = count_tokens(&out);
        let savings = 100.0 - (out_t as f64 / raw_t as f64 * 100.0);
        assert!(
            savings >= 30.0,
            "Expected ≥30% savings, got {:.1}% (raw={raw_t}, filtered={out_t})\nOutput:\n{out}",
            savings
        );
    }
}
