#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Picker {
    _temp: TempDir,
    root: PathBuf,
    config: PathBuf,
    state: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    folder: PathBuf,
}

fn executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Picker {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let config = root.join("config.toml");
        let state = root.join("state");
        let bin = root.join("bin");
        let log = root.join("rex.log");
        let folder = root.join("project with spaces");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&folder).unwrap();
        fs::write(
            &config,
            format!(
                "backend = \"rex\"\nrex_picker_split = true\nroots = [{}]\n",
                toml::Value::String(folder.to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        // Any session listing, creation, app action, or window creation fails.
        executable(
            &bin.join("rex"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$SHIREI_TEST_REX_LOG\"\n[ \"$1\" = split ] || exit 99\nexit \"${SHIREI_TEST_REX_EXIT:-0}\"\n",
        );
        executable(
            &bin.join("fzf"),
            "#!/bin/sh\nif [ \"${SHIREI_TEST_CANCEL:-0}\" = 1 ]; then exit 130; fi\nhead -n 1\n",
        );
        Self {
            _temp: temp,
            root,
            config,
            state,
            bin,
            log,
            folder,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shirei"));
        command
            .arg("pick")
            .current_dir(&self.root)
            .env("SHIREI_CONFIG", &self.config)
            .env("SHIREI_STATE_DIR", &self.state)
            .env("SHIREI_TEST_REX_LOG", &self.log)
            .env("SHIREI_TEST_REX_EXIT", "0")
            .env("SHIREI_TEST_CANCEL", "0")
            .env("REX_SESSION", "session:test")
            .env("REX_BLOCK", "block:picker")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        command
    }

    fn assert_success(&self, output: &Output) {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn folder_selection_opens_a_split_without_switching_or_creating_sessions() {
    let picker = Picker::new();
    let output = picker.command().output().unwrap();
    picker.assert_success(&output);
    assert_eq!(
        fs::read_to_string(&picker.log).unwrap(),
        format!(
            "split\n--session\nsession:test\n--block\nblock:picker\n--cwd\n{}\n--json\n",
            picker.folder.display()
        )
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(picker.state.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["recent"][0]["path"], picker.folder.to_str().unwrap());
}

#[test]
fn split_mode_requires_a_rex_session() {
    let picker = Picker::new();
    let output = picker.command().env_remove("REX_SESSION").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("inside a Rex terminal"));
    assert!(!picker.log.exists());
}

#[test]
fn split_failure_does_not_record_the_directory_as_recent() {
    let picker = Picker::new();
    let output = picker
        .command()
        .env("SHIREI_TEST_REX_EXIT", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("rex split"));
    assert!(!picker.state.join("state.json").exists());
}

#[test]
fn cancelling_the_picker_does_not_open_a_split() {
    let picker = Picker::new();
    let output = picker
        .command()
        .env("SHIREI_TEST_CANCEL", "1")
        .output()
        .unwrap();
    picker.assert_success(&output);
    assert!(!picker.log.exists());
    assert!(!picker.state.join("state.json").exists());
}
