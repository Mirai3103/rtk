//! Filters `flutter test -r json` output — shows only failures and summary.
//!
//! Injects `-r json` (machine-readable JSONL via package:test protocol) then parses
//! the event stream to suppress progress/debug noise and surface only failures.

use crate::core::runner::{self, RunOptions};
use crate::core::truncate::CAP_ERRORS;
use crate::core::utils::resolved_command;
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

lazy_static! {
    static ref RE_STACK_LINE: Regex =
        Regex::new(r"^(package:|#\d+\s|<asynchronous)").unwrap();
    static ref RE_EXCEPTION_HEADER: Regex =
        Regex::new(r"^══╡ EXCEPTION CAUGHT BY FLUTTER").unwrap();
    static ref RE_EXCEPTION_THROWN: Regex =
        Regex::new(r"^When the exception was thrown").unwrap();
    static ref RE_EXCEPTION_FOOTER: Regex = Regex::new(r"^════+$").unwrap();
    static ref RE_THROWN_DESCRIPTION: Regex =
        Regex::new(r"^The following \S+ was thrown").unwrap();
}

struct TestRecord {
    name: String,
    suite_id: u64,
    root_url: Option<String>,
    url: Option<String>,
}

struct FailureRecord {
    name: String,
    file: String,
    error_lines: Vec<String>,
}

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("flutter");
    cmd.arg("test");

    let has_reporter = args
        .iter()
        .any(|a| a == "-r" || a == "--reporter" || a.starts_with("--reporter="));

    if !has_reporter {
        cmd.arg("-r").arg("json");
    }

    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!(
            "Running: flutter test {}{}",
            if !has_reporter { "-r json " } else { "" },
            args.join(" ")
        );
    }

    runner::run_filtered(
        cmd,
        "flutter test",
        &args.join(" "),
        filter_flutter_test_json,
        RunOptions::stdout_only().tee("flutter_test"),
    )
}

fn filter_flutter_test_json(raw: &str) -> String {
    let mut suites: HashMap<u64, String> = HashMap::new();
    let mut tests: HashMap<u64, TestRecord> = HashMap::new();
    let mut errors: HashMap<u64, Vec<String>> = HashMap::new();
    let mut prints: HashMap<u64, Vec<String>> = HashMap::new();

    let mut pass: usize = 0;
    let mut fail: usize = 0;
    let mut skip: usize = 0;
    let mut elapsed_ms: u64 = 0;
    let mut failures: Vec<FailureRecord> = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        match val.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "suite" => {
                if let (Some(id), Some(path)) = (
                    val["suite"]["id"].as_u64(),
                    val["suite"]["path"].as_str(),
                ) {
                    suites.insert(id, path.to_string());
                }
            }

            "testStart" => {
                if let (Some(id), Some(name), Some(suite_id)) = (
                    val["test"]["id"].as_u64(),
                    val["test"]["name"].as_str(),
                    val["test"]["suiteID"].as_u64(),
                ) {
                    let root_url = val["test"]["root_url"].as_str().map(str::to_string);
                    let url = val["test"]["url"].as_str().map(str::to_string);
                    tests.insert(
                        id,
                        TestRecord {
                            name: name.to_string(),
                            suite_id,
                            root_url,
                            url,
                        },
                    );
                }
            }

            "print" => {
                if let (Some(id), Some(msg)) =
                    (val["testID"].as_u64(), val["message"].as_str())
                {
                    prints.entry(id).or_default().push(msg.to_string());
                }
            }

            "error" => {
                if let (Some(id), Some(error)) =
                    (val["testID"].as_u64(), val["error"].as_str())
                {
                    errors.entry(id).or_default().push(error.to_string());
                }
            }

            "testDone" => {
                let Some(id) = val["testID"].as_u64() else {
                    continue;
                };
                let hidden = val["hidden"].as_bool().unwrap_or(false);
                let skipped = val["skipped"].as_bool().unwrap_or(false);
                let result = val["result"].as_str().unwrap_or("unknown");

                if hidden {
                    continue;
                }

                if skipped {
                    skip += 1;
                } else if result == "success" {
                    pass += 1;
                } else {
                    fail += 1;

                    let rec = tests.get(&id);
                    let name = rec.map(|t| t.name.as_str()).unwrap_or("unknown");
                    let suite_id = rec.map(|t| t.suite_id).unwrap_or(0);

                    let file = rec
                        .and_then(|t| t.root_url.as_deref().or(t.url.as_deref()))
                        .or_else(|| suites.get(&suite_id).map(|s| s.as_str()))
                        .map(relative_path)
                        .unwrap_or_default();

                    let err_msgs = errors.remove(&id).unwrap_or_default();
                    let print_msgs = prints.remove(&id).unwrap_or_default();

                    let error_lines = build_error_lines(result, &err_msgs, &print_msgs);

                    failures.push(FailureRecord {
                        name: name.to_string(),
                        file,
                        error_lines,
                    });
                }
            }

            "done" => {
                if let Some(t) = val["time"].as_u64() {
                    elapsed_ms = t;
                }
            }

            _ => {}
        }
    }

    format_output(pass, fail, skip, elapsed_ms, &failures)
}

/// Extract displayable error lines from a test failure.
///
/// - `result="failure"`: matcher failure — `error` field has Expected/Actual lines
/// - `result="error"`: widget exception — actual error is inside a `print` event
fn build_error_lines(result: &str, errors: &[String], prints: &[String]) -> Vec<String> {
    if result == "failure" {
        if let Some(err) = errors.first() {
            return err
                .lines()
                .filter(|l| !RE_STACK_LINE.is_match(l.trim()))
                .take(8)
                .map(|l| format!("  {}", l))
                .collect();
        }
    } else {
        // Widget exception: look for the ══╡ EXCEPTION ╞══ print message
        for msg in prints {
            if RE_EXCEPTION_HEADER.is_match(msg) {
                return extract_exception_body(msg);
            }
        }
        // Fallback: use the error field (usually "Test failed. See exception logs above.")
        if let Some(err) = errors.first() {
            return err
                .lines()
                .take(3)
                .map(|l| format!("  {}", l))
                .collect();
        }
    }
    vec![]
}

/// Extract the meaningful body from a Flutter test framework exception message.
///
/// The message looks like:
/// ```text
/// ══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞════
/// The following TestFailure was thrown running a test:
/// Expected: exactly one matching candidate    ← keep
///   ...                                        ← keep
/// When the exception was thrown...             ← stop here
/// ```
fn extract_exception_body(msg: &str) -> Vec<String> {
    let mut in_body = false;
    let mut lines = Vec::new();

    for line in msg.lines() {
        if RE_EXCEPTION_HEADER.is_match(line) {
            continue;
        }
        if RE_THROWN_DESCRIPTION.is_match(line) {
            in_body = true;
            continue;
        }
        if RE_EXCEPTION_THROWN.is_match(line) || RE_EXCEPTION_FOOTER.is_match(line) {
            break;
        }
        if in_body {
            lines.push(format!("  {}", line));
            if lines.len() >= 8 {
                break;
            }
        }
    }
    lines
}

fn relative_path(path: &str) -> String {
    // Strip file:// prefix (widget test root_url is "file:///abs/path/test/foo.dart")
    let path = path.strip_prefix("file://").unwrap_or(path);
    // Return path starting from "test/" component
    if let Some(idx) = path.find("/test/") {
        return path[idx + 1..].to_string();
    }
    // Already relative (starts with "test/")
    if path.starts_with("test/") {
        return path.to_string();
    }
    // Fallback: just the filename
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn format_elapsed(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn format_output(
    pass: usize,
    fail: usize,
    skip: usize,
    elapsed_ms: u64,
    failures: &[FailureRecord],
) -> String {
    let elapsed = format_elapsed(elapsed_ms);
    let mut out = String::new();

    if fail == 0 {
        out.push_str(&format!("Flutter test: {} passed", pass));
        if skip > 0 {
            out.push_str(&format!(", {} skipped", skip));
        }
        out.push_str(&format!(" ({})\n", elapsed));
        return out;
    }

    out.push_str(&format!("Flutter test: {} passed, {} failed", pass, fail));
    if skip > 0 {
        out.push_str(&format!(", {} skipped", skip));
    }
    out.push_str(&format!(" ({})\n", elapsed));

    let shown = failures.iter().take(CAP_ERRORS);
    for f in shown {
        out.push('\n');
        out.push_str(&format!("[FAIL] {}\n", f.name));
        out.push_str(&format!("  file: {}\n", f.file));
        for line in &f.error_lines {
            out.push_str(line);
            out.push('\n');
        }
    }

    let total = failures.len();
    if total > CAP_ERRORS {
        out.push_str(&format!("\n... and {} more failures\n", total - CAP_ERRORS));
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
    fn test_all_pass() {
        let raw = r#"{"protocolVersion":"0.1.1","type":"start","time":0}
{"suite":{"id":0,"platform":"vm","path":"/proj/test/unit_test.dart"},"type":"suite","time":0}
{"test":{"id":1,"name":"loading","suiteID":0,"groupIDs":[]},"type":"testStart","time":0}
{"testID":1,"result":"success","hidden":true,"skipped":false,"type":"testDone","time":10}
{"test":{"id":2,"name":"adds two numbers","suiteID":0,"groupIDs":[]},"type":"testStart","time":10}
{"testID":2,"result":"success","hidden":false,"skipped":false,"type":"testDone","time":20}
{"test":{"id":3,"name":"subtracts numbers","suiteID":0,"groupIDs":[]},"type":"testStart","time":20}
{"testID":3,"result":"success","hidden":false,"skipped":false,"type":"testDone","time":30}
{"success":true,"type":"done","time":1500}"#;

        let out = filter_flutter_test_json(raw);
        assert!(out.contains("Flutter test: 2 passed"), "got: {out}");
        assert!(out.contains("1.5s"), "got: {out}");
        assert!(!out.contains("[FAIL]"), "got: {out}");
    }

    #[test]
    fn test_matcher_failure() {
        let raw = r#"{"protocolVersion":"0.1.1","type":"start","time":0}
{"suite":{"id":0,"platform":"vm","path":"/proj/test/unit_test.dart"},"type":"suite","time":0}
{"test":{"id":1,"name":"loading","suiteID":0,"groupIDs":[]},"type":"testStart","time":0}
{"testID":1,"result":"success","hidden":true,"skipped":false,"type":"testDone","time":10}
{"test":{"id":2,"name":"User model age validation fails for negative age","suiteID":0,"groupIDs":[]},"type":"testStart","time":10}
{"testID":2,"error":"Expected: a value greater than <0>\n  Actual: <-1>\n   Which: is not a value greater than <0>\nAge should be positive but was -1\n","stackTrace":"package:matcher expect\ntest/unit/user_test.dart 37:7 main\n","isFailure":true,"type":"error","time":50}
{"testID":2,"result":"failure","hidden":false,"skipped":false,"type":"testDone","time":51}
{"success":false,"type":"done","time":500}"#;

        let out = filter_flutter_test_json(raw);
        assert!(out.contains("0 passed, 1 failed"), "got: {out}");
        assert!(out.contains("[FAIL] User model age validation"), "got: {out}");
        assert!(out.contains("Expected: a value greater than <0>"), "got: {out}");
        assert!(out.contains("Actual: <-1>"), "got: {out}");
        // Stack trace must be stripped
        assert!(!out.contains("package:matcher"), "stack trace leaked: {out}");
        assert!(!out.contains("37:7"), "stack trace leaked: {out}");
    }

    #[test]
    fn test_widget_exception_failure() {
        let exception_print = "══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞════\nThe following TestFailure was thrown running a test:\nExpected: exactly one matching candidate\n  Actual: _TextWidgetFinder:<Found 0 widgets>\n   Which: means none were found\nExpected counter to start at 5\n\nWhen the exception was thrown, this was the stack:\n#4 main (file:///proj/test/widget_test.dart:57:7)\n════════════════════════════════════════════════════";
        let print_line = format!(
            "{{\"testID\":1,\"messageType\":\"print\",\"message\":{},\"type\":\"print\",\"time\":100}}",
            serde_json::to_string(exception_print).unwrap()
        );

        let raw = format!(
            r#"{{"protocolVersion":"0.1.1","type":"start","time":0}}
{{"suite":{{"id":0,"platform":"vm","path":"/proj/test/widget_test.dart"}},"type":"suite","time":0}}
{{"test":{{"id":1,"name":"Counter widget counter starts at 5","suiteID":0,"groupIDs":[],"root_url":"file:///proj/test/widget_test.dart"}},"type":"testStart","time":0}}
{}
{{"testID":1,"error":"Test failed. See exception logs above.","stackTrace":"","isFailure":false,"type":"error","time":110}}
{{"testID":1,"result":"error","hidden":false,"skipped":false,"type":"testDone","time":111}}
{{"success":false,"type":"done","time":1000}}"#,
            print_line
        );

        let out = filter_flutter_test_json(&raw);
        assert!(out.contains("[FAIL] Counter widget counter starts at 5"), "got: {out}");
        assert!(out.contains("Expected: exactly one matching candidate"), "got: {out}");
        assert!(out.contains("file: test/widget_test.dart"), "got: {out}");
        // Stack frames must be stripped
        assert!(!out.contains("#4 main"), "stack frame leaked: {out}");
    }

    #[test]
    fn test_skip_reporter_injection() {
        // Verifies that -r is detected (the run() function handles this — unit test the flag detection)
        let args_with_reporter = ["-r".to_string(), "expanded".to_string()];
        let has_reporter = args_with_reporter
            .iter()
            .any(|a| a == "-r" || a == "--reporter" || a.starts_with("--reporter="));
        assert!(has_reporter, "should detect -r flag");

        let args_without = ["test/unit/".to_string()];
        let no_reporter = args_without
            .iter()
            .any(|a| a == "-r" || a == "--reporter" || a.starts_with("--reporter="));
        assert!(!no_reporter, "should not detect reporter flag");
    }

    #[test]
    fn test_token_savings_real_fixture() {
        let raw =
            include_str!("../../../tests/fixtures/flutter/flutter_test_json_raw.txt");
        let out = filter_flutter_test_json(raw);

        let raw_tokens = count_tokens(raw);
        let out_tokens = count_tokens(&out);
        let savings = 100.0 - (out_tokens as f64 / raw_tokens as f64 * 100.0);

        // JSONL input is already compact (no verbose text noise), so 60% is the bar.
        // Real-world savings vs plain-text flutter test output are ~85%+.
        assert!(
            savings >= 60.0,
            "Expected ≥60% token savings, got {:.1}% (raw={raw_tokens}, filtered={out_tokens})\nFiltered output:\n{out}",
            savings
        );
    }

    #[test]
    fn test_real_fixture_failures_present() {
        let raw =
            include_str!("../../../tests/fixtures/flutter/flutter_test_json_raw.txt");
        let out = filter_flutter_test_json(raw);

        assert!(out.contains("Flutter test:"), "missing header: {out}");
        assert!(out.contains("failed"), "should report failures: {out}");
        assert!(out.contains("[FAIL]"), "should list failures: {out}");
        assert!(out.contains("file: test/"), "should show relative path: {out}");
        // Progress lines and debug prints must be gone
        assert!(!out.contains("Fetching user"), "debug print leaked: {out}");
        assert!(!out.contains("Counter incremented"), "debug print leaked: {out}");
    }

    #[test]
    fn test_relative_path_stripping() {
        assert_eq!(
            relative_path("file:///home/user/project/test/unit/user_test.dart"),
            "test/unit/user_test.dart"
        );
        assert_eq!(
            relative_path("/home/user/project/test/widget_test.dart"),
            "test/widget_test.dart"
        );
        assert_eq!(relative_path("test/simple_test.dart"), "test/simple_test.dart");
    }

    #[test]
    fn test_with_skip() {
        let raw = r#"{"type":"start","time":0}
{"suite":{"id":0,"platform":"vm","path":"/proj/test/t.dart"},"type":"suite","time":0}
{"test":{"id":1,"name":"loading","suiteID":0,"groupIDs":[]},"type":"testStart","time":0}
{"testID":1,"result":"success","hidden":true,"skipped":false,"type":"testDone","time":5}
{"test":{"id":2,"name":"skipped test","suiteID":0,"groupIDs":[]},"type":"testStart","time":5}
{"testID":2,"result":"success","hidden":false,"skipped":true,"type":"testDone","time":6}
{"test":{"id":3,"name":"passing test","suiteID":0,"groupIDs":[]},"type":"testStart","time":6}
{"testID":3,"result":"success","hidden":false,"skipped":false,"type":"testDone","time":10}
{"success":true,"type":"done","time":200}"#;

        let out = filter_flutter_test_json(raw);
        assert!(out.contains("1 passed"), "got: {out}");
        assert!(out.contains("1 skipped"), "got: {out}");
    }
}
