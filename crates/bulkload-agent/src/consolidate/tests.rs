//! The structured-config union (operator ruling 2026-10-20, item 2): one
//! row per class rule, then the crash-safe write against a live writer.

use std::cell::RefCell;
use std::ffi::CStr;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::config::{consolidate, merge, Contents, Format, Target};
use super::value::{pointer, Maps, Side};
use super::{json, toml, Action, Ledger};
use crate::BulkloadRefusal;

fn maps() -> Maps {
    Maps::new([
        ("/Users/jess".to_owned(), "/home/jess".to_owned()),
        ("/Users/jess/git".to_owned(), "/home/jess/git".to_owned()),
    ])
}

/// The source modified after the live file, and before it.
const LATER: (i128, i128) = (1_000, 2_000);
const EARLIER: (i128, i128) = (2_000, 1_000);

fn run(
    format: Format,
    live: &str,
    source: &str,
    (live_ns, source_ns): (i128, i128),
) -> super::config::Merged {
    merge(
        format,
        Contents {
            bytes: live.as_bytes(),
            mtime_ns: live_ns,
        },
        Contents {
            bytes: source.as_bytes(),
            mtime_ns: source_ns,
        },
        &maps(),
    )
}

fn written(merged: &super::config::Merged) -> String {
    String::from_utf8(merged.bytes.clone().expect("bytes written")).unwrap()
}

// ---- JSON ------------------------------------------------------------------

#[test]
fn json_stringify_documents_round_trip_byte_for_byte() {
    let documents = [
        "{\n  \"permissions\": {\n    \"allow\": [\n      \"Bash(git status:*)\"\n    ],\n    \"deny\": []\n  },\n  \"hooks\": {},\n  \"model\": \"opus\",\n  \"n\": -1.5e+3,\n  \"t\": true,\n  \"z\": null\n}\n",
        "{\n    \"a\": [\n        1,\n        {\n            \"b\": \"c\\\"d\\\\e\\n\\u0001\"\n        }\n    ]\n}",
        "{\n\t\"tab\": \"\u{e9}\u{1f600}\"\n}\n",
        "{\"compact\":[1,2,{\"x\":0}],\"y\":\"z\"}",
        "{\"spaced\": [1, 2], \"y\": \"z\"}\n",
        "{\r\n  \"crlf\": [\r\n    1\r\n  ]\r\n}\r\n",
        "[\n  \"top\",\n  []\n]\n",
    ];
    for document in documents {
        let value = json::parse(document).unwrap();
        assert_eq!(json::write(&value, &json::Style::of(document)), document);
    }
}

const STING_SETTINGS: &str = r#"{
  "permissions": {
    "allow": [
      "Bash(git status:*)",
      "Read(/home/jess/git/**)"
    ],
    "defaultMode": "acceptEdits"
  },
  "model": "opus",
  "cleanupPeriodDays": 3650
}
"#;

const NEO_SETTINGS: &str = r#"{
  "cleanupPeriodDays": 3650,
  "permissions": {
    "defaultMode": "acceptEdits",
    "allow": [
      "Read(/Users/jess/git/**)",
      "Bash(cargo fmt:*)",
      "Bash(git status:*)"
    ]
  },
  "statusLine": {
    "type": "command",
    "command": "/Users/jess/.claude/statusline.sh"
  }
}
"#;

#[test]
fn json_union_merges_objects_key_by_key_and_unions_arrays_by_value() {
    let merged = run(Format::Json, STING_SETTINGS, NEO_SETTINGS, EARLIER);
    assert_eq!(merged.action, Action::Consolidated);
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    // The live file's order, then the source's new keys; the array's live
    // elements, then the one new element (its path translated, so the
    // translated duplicate is not a second element).
    assert_eq!(
        written(&merged),
        r#"{
  "permissions": {
    "allow": [
      "Bash(git status:*)",
      "Read(/home/jess/git/**)",
      "Bash(cargo fmt:*)"
    ],
    "defaultMode": "acceptEdits"
  },
  "model": "opus",
  "cleanupPeriodDays": 3650,
  "statusLine": {
    "type": "command",
    "command": "/home/jess/.claude/statusline.sh"
  }
}
"#
    );
    assert_eq!(merged.keys_added, 1);
    assert_eq!(merged.elements_added, 1);
}

#[test]
fn json_a_key_both_hold_differently_takes_the_later_side_and_records_the_other() {
    let live = "{\n  \"model\": \"opus\",\n  \"theme\": \"dark\"\n}\n";
    let source = "{\n  \"model\": \"sonnet\",\n  \"theme\": \"dark\"\n}\n";
    // The source modified last: its value is in the file; the live value
    // is the recorded one.
    let merged = run(Format::Json, live, source, LATER);
    assert_eq!(merged.action, Action::Consolidated);
    assert_eq!(
        written(&merged),
        "{\n  \"model\": \"sonnet\",\n  \"theme\": \"dark\"\n}\n"
    );
    assert_eq!(merged.conflicts.len(), 1);
    let conflict = &merged.conflicts[0];
    assert_eq!(pointer(&conflict.path), "/model");
    assert_eq!(conflict.kept_side, Side::Source);
    assert_eq!(json::compact(&conflict.other), "\"opus\"");
    // The live file modified last: nothing to write; the source's value is
    // the recorded one.
    let merged = run(Format::Json, live, source, EARLIER);
    assert_eq!(merged.action, Action::Covered);
    assert!(merged.bytes.is_none());
    assert_eq!(merged.conflicts[0].kept_side, Side::Live);
    assert_eq!(json::compact(&merged.conflicts[0].other), "\"sonnet\"");
    // A key whose kind differs is a conflict like any other value.
    let merged = run(
        Format::Json,
        "{\"hooks\":{\"Stop\":[]}}",
        "{\"hooks\":\"off\"}",
        LATER,
    );
    assert_eq!(written(&merged), "{\"hooks\":\"off\"}");
    assert_eq!(json::compact(&merged.conflicts[0].other), "{\"Stop\":[]}");
}

#[test]
fn json_source_paths_are_destination_paths_before_comparing() {
    // ~/.claude.json keys its projects by path: neo's project is sting's,
    // merged key by key, never a second project.
    let live = r#"{
  "numStartups": 12,
  "projects": {
    "/home/jess/git/lab": {
      "allowedTools": [],
      "hasTrustDialogAccepted": true
    }
  }
}
"#;
    let source = r#"{
  "numStartups": 40,
  "projects": {
    "/Users/jess/git/lab": {
      "allowedTools": ["Bash"],
      "hasTrustDialogAccepted": true
    },
    "/Users/jessie/x": {}
  }
}
"#;
    let merged = run(Format::Json, live, source, LATER);
    assert_eq!(
        written(&merged),
        r#"{
  "numStartups": 40,
  "projects": {
    "/home/jess/git/lab": {
      "allowedTools": [
        "Bash"
      ],
      "hasTrustDialogAccepted": true
    },
    "/Users/jessie/x": {}
  }
}
"#
    );
    assert_eq!(merged.conflicts.len(), 1);
    assert_eq!(pointer(&merged.conflicts[0].path), "/numStartups");
}

#[test]
fn json_what_is_not_carried_losslessly_is_held_and_nothing_written() {
    let live = "{\"a\": 1}\n";
    for (source, reason) in [
        ("{\"a\": 1, \"a\": 2}", "json: duplicate key"),
        ("{\"a\": 1,}", "json: malformed"),
        ("\u{feff}{\"a\": 2}", "json: byte-order mark"),
        ("{\"a\": 2} // note", "json: bytes after the document"),
        ("{\"a\": \"\\ud800\"}", "json: lone surrogate"),
        (
            "{\"/Users/jess/x\": 1, \"/home/jess/x\": 2}",
            "two source keys translate to one",
        ),
    ] {
        let merged = run(Format::Json, live, source, LATER);
        assert_eq!(merged.action, Action::Held(reason), "{source}");
        assert_eq!(
            merged.action.refusal(),
            Some(BulkloadRefusal::HomesConsolidateUnsupported)
        );
        assert!(merged.bytes.is_none());
    }
    let merged = merge(
        Format::Json,
        Contents {
            bytes: live.as_bytes(),
            mtime_ns: 0,
        },
        Contents {
            bytes: b"{\"a\": \"\xff\"}",
            mtime_ns: 1,
        },
        &maps(),
    );
    assert_eq!(merged.action, Action::Held("not UTF-8"));
}

// ---- TOML ------------------------------------------------------------------

const STING_CODEX: &str = r#"# sting's Codex config
model = "gpt-5-codex"
approval_policy = "on-request" # asks first

[projects."/home/jess/git/lab"]
trust_level = "trusted"

[mcp_servers.linear]
command = "npx"
args = [
  "-y",
  "mcp-remote",
]
"#;

const NEO_CODEX: &str = r#"model = "gpt-5-codex"
model_reasoning_effort = "high"
approval_policy = "on-request"

[projects."/Users/jess/git/lab"]
trust_level = "trusted"

[projects."/Users/jess/git/tummycrypt"]
trust_level = "trusted"

[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/sse"]
env = { LINEAR_TEAM = "TIN" }

[tui]
notifications = true
"#;

#[test]
fn toml_keys_tables_and_elements_are_written_in_place_with_comments_kept() {
    let merged = run(Format::Toml, STING_CODEX, NEO_CODEX, EARLIER);
    assert_eq!(merged.action, Action::Consolidated);
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    assert_eq!(
        written(&merged),
        r#"# sting's Codex config
model = "gpt-5-codex"
approval_policy = "on-request" # asks first
model_reasoning_effort = "high"

[projects."/home/jess/git/lab"]
trust_level = "trusted"

[mcp_servers.linear]
command = "npx"
args = [
  "-y",
  "mcp-remote",
  "https://mcp.linear.app/sse",
]

[projects."/home/jess/git/tummycrypt"]
trust_level = "trusted"

[mcp_servers.linear.env]
LINEAR_TEAM = "TIN"

[tui]
notifications = true
"#
    );
    // Merged again, the union is covered: nothing to write.
    let again = run(Format::Toml, &written(&merged), NEO_CODEX, LATER);
    assert_eq!(again.action, Action::Covered);
}

#[test]
fn toml_a_value_the_source_wins_replaces_only_its_span() {
    let live = "model = \"o3\" # pinned\nother = 1\n";
    let source = "model = \"gpt-5\"\nother = 1\n";
    let merged = run(Format::Toml, live, source, LATER);
    assert_eq!(written(&merged), "model = \"gpt-5\" # pinned\nother = 1\n");
    assert_eq!(pointer(&merged.conflicts[0].path), "/model");
    assert_eq!(Format::Toml.one_line(&merged.conflicts[0].other), "\"o3\"");
    let merged = run(Format::Toml, live, source, EARLIER);
    assert_eq!(merged.action, Action::Covered);
    assert_eq!(
        Format::Toml.one_line(&merged.conflicts[0].other),
        "\"gpt-5\""
    );
}

#[test]
fn toml_inline_tables_dotted_keys_and_arrays_of_tables() {
    let live =
        "env = { A = \"1\" }\n\n[tui]\nstyle.theme = \"dark\"\n\n[[profiles]]\nname = \"a\"\n";
    let source = "env = { A = \"1\", B = \"2\" }\n\n[tui]\nstyle.theme = \"dark\"\nstyle.accent = \"blue\"\n\n[[profiles]]\nname = \"a\"\n\n[[profiles]]\nname = \"b\"\n";
    let merged = run(Format::Toml, live, source, EARLIER);
    assert_eq!(
        written(&merged),
        "env = { A = \"1\", B = \"2\" }\n\n[tui]\nstyle.theme = \"dark\"\nstyle.accent = \"blue\"\n\n[[profiles]]\nname = \"a\"\n\n[[profiles]]\nname = \"b\"\n"
    );
    // Single-line arrays, an empty one, and new keys of a table only deeper
    // headers name.
    let live = "a = [1, 2]\nb = []\n\n[x.y]\nz = 1\n";
    let source = "a = [2, 3]\nb = [\"q\"]\n\n[x]\nw = true\n\n[x.y]\nz = 1\n";
    let merged = run(Format::Toml, live, source, EARLIER);
    assert_eq!(
        written(&merged),
        "a = [1, 2, 3]\nb = [\"q\"]\n\n[x.y]\nz = 1\n\n[x]\nw = true\n"
    );
}

#[test]
fn toml_spellings_of_one_value_are_the_same_value() {
    let live = "a = 0x10\nb = 1_000\nc = 1979-05-27T07:32:00Z\nd = 'lit'\ne = 1.0\n";
    let source = "a = 16\nb = 1000\nc = 1979-05-27 07:32:00z\nd = \"lit\"\ne = 1.0\n";
    let merged = run(Format::Toml, live, source, LATER);
    assert_eq!(merged.action, Action::Covered, "{:?}", merged.conflicts);
    assert!(merged.conflicts.is_empty());
}

#[test]
fn toml_crlf_and_a_last_line_without_a_newline() {
    let merged = run(
        Format::Toml,
        "a = 1\r\n\r\n[t]\r\nb = 2\r\n",
        "c = 3\r\n[t]\r\nd = 4\r\n",
        EARLIER,
    );
    assert_eq!(
        written(&merged),
        "a = 1\r\nc = 3\r\n\r\n[t]\r\nb = 2\r\nd = 4\r\n"
    );
    let merged = run(Format::Toml, "a = 1", "b = 2", EARLIER);
    assert_eq!(written(&merged), "a = 1\nb = 2\n");
    // A root with no keys takes them above the first header's comments.
    let merged = run(
        Format::Toml,
        "# lead\n\n# the table\n[t]\nb = 2\n",
        "a = 1\n",
        EARLIER,
    );
    assert_eq!(
        written(&merged),
        "# lead\n\na = 1\n# the table\n[t]\nb = 2\n"
    );
}

#[test]
fn toml_what_cannot_be_written_in_place_is_held() {
    for (live, source, reason) in [
        ("a = 1\na = 2\n", "a = 1\n", "toml: duplicate key"),
        (
            "[t]\na = 1\n[t]\nb = 2\n",
            "a = 1\n",
            "toml: table defined twice",
        ),
        (
            "[x]\na = 1\n",
            "x = 5\n",
            "toml: a table defined by headers changes kind",
        ),
        ("a = \n", "a = 1\n", "toml: missing value"),
        ("a = yes\n", "a = 1\n", "toml: malformed value"),
        ("a = 1\n", "b = 01\n", "toml: malformed value"),
    ] {
        let merged = run(Format::Toml, live, source, LATER);
        assert_eq!(merged.action, Action::Held(reason), "{live:?} {source:?}");
        assert!(merged.bytes.is_none());
    }
}

#[test]
fn toml_documents_parse_into_their_values() {
    let doc = toml::parse(
        "s = \"\"\"\nline one\nline \\\n   two\"\"\"\nl = '''\nraw\\n'''\nq = \"\"\"a\"\"\"\"\nf = [ -inf, nan, 6.02e23 ]\nt = 07:32:00\n",
    )
    .unwrap();
    assert_eq!(
        json::compact(&doc.value),
        "{\"s\":\"line one\\nline two\",\"l\":\"raw\\\\n\",\"q\":\"a\\\"\",\"f\":[-inf,nan,6.02e23],\"t\":07:32:00}"
    );
}

// ---- the write ---------------------------------------------------------------

thread_local! {
    static BEFORE_SWAP: RefCell<Option<Box<dyn FnMut()>>> = RefCell::new(None);
}

/// Runs between the merged bytes' durability and the trade.
pub(super) fn before_swap(_dir: &File, _name: &CStr) {
    BEFORE_SWAP.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

pub(super) fn set_hook(hook: Option<Box<dyn FnMut()>>) {
    BEFORE_SWAP.with(|slot| *slot.borrow_mut() = hook);
}

struct Home {
    root: PathBuf,
}

impl Home {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bulkload-consolidate-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    fn live(&self, name: &str, text: &str, age: Duration) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
        path
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        set_hook(None);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn now_ns() -> i128 {
    i128::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn consolidate_into(
    home: &Home,
    ledger: &Ledger,
    name: &str,
    source: &str,
    source_ns: i128,
) -> crate::Result<super::config::Outcome> {
    let dir = File::open(&home.root).unwrap();
    consolidate(
        &Target {
            dir: &dir,
            name: name.as_bytes(),
            rel: format!(".claude/{name}").as_bytes(),
        },
        Format::of(name.as_bytes()).unwrap(),
        Contents {
            bytes: source.as_bytes(),
            mtime_ns: source_ns,
        },
        &maps(),
        ledger,
    )
}

#[test]
fn the_union_is_traded_in_for_the_live_file_and_its_conflicts_recorded() {
    let home = Home::new("write");
    let ledger = Ledger::open(&home.root.join("state")).unwrap();
    let live = "{\n  \"model\": \"opus\",\n  \"a\": [\n    1\n  ]\n}\n";
    let path = home.live("settings.json", live, Duration::from_hours(1));
    let before = fs::metadata(&path).unwrap();
    let source = "{\n  \"model\": \"sonnet\",\n  \"a\": [\n    2\n  ]\n}\n";
    let outcome = consolidate_into(&home, &ledger, "settings.json", source, now_ns()).unwrap();
    assert_eq!(outcome.action, Action::Consolidated);
    assert_eq!(outcome.attempts, 1);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\n  \"model\": \"sonnet\",\n  \"a\": [\n    1,\n    2\n  ]\n}\n"
    );
    let after = fs::metadata(&path).unwrap();
    assert_ne!(
        after.ino(),
        before.ino(),
        "a new file traded in, not rewritten"
    );
    assert_eq!(after.mode() & 0o7777, 0o600);
    assert_eq!(
        home.names(),
        ["settings.json", "state"],
        "no private name left"
    );
    let recorded = ledger.recorded().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].file, b".claude/settings.json");
    assert_eq!(recorded[0].key_path, "/model");
    assert_eq!(recorded[0].other_side, "live");
    assert_eq!(recorded[0].other, "\"opus\"");
    assert_eq!(recorded[0].kept, "\"sonnet\"");
    // Again with the same source: covered, nothing written, nothing
    // recorded twice.
    let again = consolidate_into(&home, &ledger, "settings.json", source, 1).unwrap();
    assert_eq!(again.action, Action::Covered);
    assert_eq!(fs::metadata(&path).unwrap().ino(), after.ino());
    assert_eq!(ledger.recorded().unwrap().len(), 1);
}

#[test]
fn a_writer_that_changes_the_file_mid_merge_is_merged_again() {
    let home = Home::new("writer");
    let ledger = Ledger::open(&home.root.join("state")).unwrap();
    let path = home.live(
        "settings.json",
        "{\n  \"a\": 1\n}\n",
        Duration::from_mins(1),
    );
    let writer = path.clone();
    let mut fired = false;
    set_hook(Some(Box::new(move || {
        if !fired {
            fired = true;
            // The destination's own writer, writing in place.
            fs::write(&writer, "{\n  \"a\": 1,\n  \"writer\": true\n}\n").unwrap();
        }
    })));
    let outcome =
        consolidate_into(&home, &ledger, "settings.json", "{\n  \"b\": 2\n}\n", 1).unwrap();
    assert_eq!(outcome.action, Action::Consolidated);
    assert_eq!(outcome.attempts, 2);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\n  \"a\": 1,\n  \"writer\": true,\n  \"b\": 2\n}\n"
    );
    assert_eq!(home.names(), ["settings.json", "state"]);
}

#[test]
fn a_writer_that_replaces_the_file_on_every_read_is_raced_and_nothing_changes() {
    let home = Home::new("raced");
    let ledger = Ledger::open(&home.root.join("state")).unwrap();
    let path = home.live("config.toml", "a = 1\n", Duration::from_mins(1));
    let writer = path.clone();
    let mut round = 0;
    set_hook(Some(Box::new(move || {
        round += 1;
        // Written beside and renamed over, as an editor does.
        let temp = writer.with_extension("tmp");
        fs::write(&temp, format!("a = 1\nround = {round}\n")).unwrap();
        fs::rename(&temp, &writer).unwrap();
    })));
    let refused = consolidate_into(&home, &ledger, "config.toml", "b = 2\n", 1).unwrap_err();
    assert_eq!(refused, BulkloadRefusal::HomesConsolidateRaced);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("a = 1\nround = {}\n", super::config::ATTEMPTS)
    );
    assert_eq!(home.names(), ["config.toml", "state"]);
    assert!(ledger.recorded().unwrap().is_empty());
}

#[test]
fn a_link_is_never_followed_an_absent_file_is_created_and_a_held_one_untouched() {
    let home = Home::new("kinds");
    let ledger = Ledger::open(&home.root.join("state")).unwrap();
    let elsewhere = home.root.join("elsewhere.json");
    fs::write(&elsewhere, "{\"x\": 1}").unwrap();
    std::os::unix::fs::symlink(&elsewhere, home.root.join("linked.json")).unwrap();
    let refused = consolidate_into(&home, &ledger, "linked.json", "{\"y\": 2}", 1).unwrap_err();
    assert_eq!(refused, BulkloadRefusal::DestinationKindMismatch);
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "{\"x\": 1}");

    let created = consolidate_into(&home, &ledger, "new.json", "{\"y\": 2}\n", 1).unwrap();
    assert_eq!(created.action, Action::Created);
    assert_eq!(
        fs::read_to_string(home.root.join("new.json")).unwrap(),
        "{\"y\": 2}\n"
    );

    let path = home.live("held.json", "{\"a\": 1}\n", Duration::from_mins(1));
    let held = consolidate_into(
        &home,
        &ledger,
        "held.json",
        "{\"a\": 1, \"a\": 2}",
        now_ns(),
    )
    .unwrap();
    assert_eq!(held.action, Action::Held("json: duplicate key"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\": 1}\n");
    assert_eq!(
        home.names(),
        [
            "elsewhere.json",
            "held.json",
            "linked.json",
            "new.json",
            "state"
        ]
    );
}

#[test]
fn a_path_prefix_is_translated_only_as_a_whole_path() {
    let maps = maps();
    assert_eq!(maps.string("/Users/jess"), "/home/jess");
    assert_eq!(
        maps.string("/Users/jess/git/x y=/Users/jess:/Users/jessie"),
        "/home/jess/git/x y=/home/jess:/Users/jessie"
    );
    assert_eq!(
        maps.string("node /Users/jess/.claude/hooks/a.js"),
        "node /home/jess/.claude/hooks/a.js"
    );
    assert_eq!(maps.string("x/Users/jess"), "x/Users/jess");
}

#[test]
fn files_are_classed_by_the_union_they_take() {
    use super::{union_of, Class};
    assert_eq!(
        Class::of(b".claude/projects/k/s.jsonl", false),
        Class::AppendLog
    );
    assert_eq!(Class::of(b"history.jsonl", false), Class::AppendLog);
    assert_eq!(
        Class::of(b".claude/settings.json", false),
        Class::Config(Format::Json)
    );
    assert_eq!(
        Class::of(b"config.toml", false),
        Class::Config(Format::Toml)
    );
    assert_eq!(Class::of(b"state_5.sqlite", false), Class::Sqlite);
    assert_eq!(Class::of(b"anything", true), Class::Sqlite);
    assert_eq!(Class::of(b"state_5.sqlite-wal", false), Class::Sqlite);
    assert_eq!(Class::of(b"logs_1.sqlite-journal", false), Class::Sqlite);
    assert_eq!(Class::of(b"notes-wal", false), Class::Opaque);
    assert_eq!(Class::of(b"plugins/cache/x.bin", false), Class::Opaque);
    // The append-log union is the #294 lane's; the SQLite union is the
    // #278 lane's, and until it lands that class is held.
    assert!(union_of(Class::AppendLog).is_some());
    assert!(union_of(Class::Sqlite).is_none());
    assert!(union_of(Class::Config(Format::Json)).is_some());
    assert!(union_of(Class::Opaque).is_some());
}

#[test]
fn an_opaque_file_is_adopted_when_equal_and_held_when_not() {
    let home = Home::new("opaque");
    let ledger = Ledger::open(&home.root.join("state")).unwrap();
    home.live("blob.bin", "same", Duration::from_mins(1));
    let dir = File::open(&home.root).unwrap();
    for (bytes, expected) in [
        ("same", Action::Adopted),
        ("other", Action::Held("opaque: the bytes differ")),
    ] {
        let outcome = super::opaque(
            &super::Live {
                dir: &dir,
                name: b"blob.bin",
                path: &home.root.join("blob.bin"),
                rel: b"blob.bin",
            },
            &super::Source {
                bytes: bytes.as_bytes(),
                mtime_ns: 1,
                maps: &maps(),
            },
            &ledger,
        )
        .unwrap();
        assert_eq!(outcome.action, expected);
        assert_eq!(
            fs::read_to_string(home.root.join("blob.bin")).unwrap(),
            "same"
        );
    }
}
