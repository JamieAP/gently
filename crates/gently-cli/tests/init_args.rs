use assert_cmd::Command;

#[test]
fn conflicting_harness_flags_fail_before_writing_configuration() {
    let home = tempfile::tempdir().unwrap();
    let result = Command::cargo_bin("gently")
        .unwrap()
        .env_clear()
        .env("HOME", home.path())
        .args(["init", "--claude", "--codex"])
        .assert()
        .failure();
    assert!(result.get_output().stdout.is_empty());
    for path in [".claude", ".codex", ".gently"] {
        assert!(!home.path().join(path).exists());
    }
}
