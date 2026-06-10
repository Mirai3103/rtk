# Flutter Test Filter — Implementation Checklist

## Context

Implement `rtk flutter test` filter. Approach: inject `-r json` into `flutter test`
to get structured JSONL output from the `package:test` protocol, then parse and compact
to a token-efficient summary. Mirrors how `rtk go test` uses `-json`.

Reference fixture: `tests/fixtures/flutter/flutter_test_json_raw.txt`
Reference sample (text format): `/home/laffy/claude-workspace/rtk_flutter/samples/flutter_test.txt`

---

## TODO

- [x] Capture `flutter test -r json` fixture → `tests/fixtures/flutter/flutter_test_json_raw.txt`
- [x] Create `src/cmds/flutter/mod.rs`
- [x] Create `src/cmds/flutter/flutter_test_cmd.rs`
  - [x] Serde structs for `package:test` JSON protocol events
  - [x] `filter_flutter_test_json(raw: &str) -> String`
  - [x] `pub fn run(args: &[String], verbose: u8) -> Result<i32>`
  - [x] Tests: `test_all_pass`, `test_with_failures`, `test_widget_failure`, `test_skip_reporter_injection`, `test_token_savings`
- [x] Update `src/cmds/mod.rs` — add `pub mod flutter;`
- [x] Update `src/main.rs`
  - [x] Import `flutter_test_cmd`
  - [x] Add `FlutterCommands` enum
  - [x] Add `Flutter` variant to `Commands` enum
  - [x] Add routing match arm
- [x] Update `src/discover/rules.rs` — add `RtkRule` for `flutter test`
- [x] `cargo fmt --all && cargo clippy --all-targets && cargo test --all` — 2158 passed

---

## Output Format

All passed:
```
Flutter test: 14 passed (7.9s)
```

With failures:
```
Flutter test: 11 passed, 3 failed (7.9s)

[FAIL] User model age validation fails for negative age
  file: test/unit/user_test.dart
  Expected: a value greater than <0>
    Actual: <-1>
     Which: is not a value greater than <0>
  Age should be positive but was -1

[FAIL] Counter widget counter starts at 5 - intentional failure
  file: test/widget_test.dart
  Expected: exactly one matching candidate
    Actual: _TextWidgetFinder:<Found 0 widgets with text "5": []>
   Which: means none were found but one was expected
  Expected counter to start at 5, but it starts at 0
```

---

## Key JSON Events (package:test protocol)

| Event type   | Action |
|--------------|--------|
| `suite`      | Map `suite.id → suite.path` |
| `testStart`  | Map `test.id → {name, suiteID, root_url, url}` |
| `print`      | Suppress (debug output). Store for widget error extraction |
| `error`      | Append `error` field to errors map by testID |
| `testDone` hidden=true | Skip (loading tests) |
| `testDone` result=success | pass++ |
| `testDone` result=failure | fail++, collect matcher error |
| `testDone` result=error   | fail++, extract from print[EXCEPTION] |
| `testDone` skipped=true   | skip++ |
| `done`       | Extract elapsed `time` (ms) |

## Notes

- Widget test `testStart.url` = `package:flutter_test/...` — use `root_url` instead for the real file path
- Stack trace lines in `error` field start with `package:` or `#N ` — strip them
- Widget failures arrive as `result="error"` with `isFailure=false`; actual error is in the `print` event containing `══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞`
- Token savings target: ≥ 80%
