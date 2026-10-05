//! Configuration acceptance runs in disposable homes without inherited secrets.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::{contains, is_empty};
use std::path::Path;

fn command(state: &Path) -> Command {
    let mut cmd = Command::cargo_bin("gently").unwrap();
    cmd.env_clear()
        .env("HOME", state)
        .env("PATH", "/usr/bin:/bin")
        .env("GENTLY_STATE_DIR", state);
    cmd
}

#[test]
fn invalid_numeric_tunables_fail_before_runtime_state_or_success_health() {
    for (field, value) in [
        ("outbox_cap", "0"),
        ("outbox_cap", "1000001"),
        ("export_batch", "0"),
        ("export_batch", "4097"),
        ("export_timeout_secs", "0"),
        ("export_timeout_secs", "3601"),
        ("query_timeout_secs", "0"),
        ("query_timeout_secs", "3601"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("{field} = {value}\n"),
        )
        .unwrap();
        command(dir.path())
            .arg("status")
            .assert()
            .failure()
            .stdout(is_empty())
            .stderr(contains(field));
        assert!(!dir.path().join("tenants").exists());
    }
}

#[test]
fn invalid_query_timeout_environment_is_rejected_instead_of_ignored() {
    for value in ["0", "3601", "not-a-duration", "-1", ""] {
        let dir = tempfile::tempdir().unwrap();
        command(dir.path())
            .arg("status")
            .env("GENTLY_QUERY_TIMEOUT_SECS", value)
            .assert()
            .failure()
            .stdout(is_empty())
            .stderr(contains("GENTLY_QUERY_TIMEOUT_SECS"));
        assert!(!dir.path().join("tenants").exists());
    }
}

#[test]
fn raw_paths_must_be_absolute_even_when_capture_or_resolution_is_disabled() {
    for (field, variable) in [
        ("raw_manifest", "GENTLY_RAW_MANIFEST"),
        ("raw_trust", "GENTLY_RAW_TRUST"),
        ("raw_identity", "GENTLY_RAW_IDENTITY"),
    ] {
        for path in ["policy.json", "../policy.json", "~/policy.json"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("config.toml"),
                format!("{field} = '{path}'\n"),
            )
            .unwrap();
            command(dir.path())
                .arg("status")
                .assert()
                .failure()
                .stderr(contains(field).and(contains("absolute")));
            std::fs::write(dir.path().join("config.toml"), "").unwrap();
            command(dir.path())
                .arg("status")
                .env(variable, path)
                .assert()
                .failure()
                .stderr(contains(field).and(contains("absolute")));
        }
    }
}

#[test]
fn public_config_resolves_file_namespace_and_environment_overrides_without_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "collector_url = 'https://example.test'\ntenant_id = 'team-blue'\ndevice_id = 'linux-main'\n").unwrap();
    let output = command(dir.path())
        .args(["config", "--json"])
        .env("GENTLY_TOKEN", "synthetic-config-token-canary")
        .env(
            "GENTLY_RAW_IDENTITY",
            dir.path().join("identity-private-path-canary"),
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("synthetic-config-token-canary"));
    assert!(!text.contains("identity-private-path-canary"));
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(config.as_object().unwrap().len(), 4);
    assert_eq!(config["tenant_id"], "team-blue");
    assert_eq!(config["device_id"], "linux-main");
    assert_eq!(config["state_dir"], dir.path().to_string_lossy().as_ref());
    assert_eq!(config["collector_url"], "https://example.test");
    assert!(!dir.path().join("tenants").exists());
    let output = command(dir.path())
        .args(["config", "--json"])
        .env("GENTLY_TENANT_ID", "team-green")
        .env("GENTLY_DEVICE_ID", "mac-main")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let overridden: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(overridden["tenant_id"], "team-green");
    assert_eq!(overridden["device_id"], "mac-main");
}

#[test]
fn configured_url_with_credentials_is_never_printed_by_public_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "collector_url = 'https://user:synthetic-url-password@example.test'\n",
    )
    .unwrap();
    let output = command(dir.path())
        .args(["config", "--json"])
        .assert()
        .failure()
        .stdout(is_empty())
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-url-password"));
}

#[test]
fn setup_check_validates_public_policy_without_reading_or_unlocking_identity() {
    use gently_raw::{DeviceIdentity, Manifest, OwnerKey, Recipient, TrustPin};
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let owner = OwnerKey::generate();
    let manifest = gently_raw::sign_manifest(
        Manifest {
            version: 1,
            tenant_id: "team-blue".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "reader-key".into(),
                recipient: identity.to_public().to_string(),
            }],
        },
        &owner,
    )
    .unwrap();
    let pin = TrustPin {
        tenant_id: "team-blue".into(),
        owner_verify_key_b64: owner.verification_key_b64(),
        min_epoch: 1,
        manifest_digest: gently_raw::manifest_digest(&manifest.manifest).unwrap(),
    };
    let manifest_path = dir.path().join("manifest.json");
    let trust_path = dir.path().join("trust.json");
    let identity_path = dir.path().join("reader.age");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(&trust_path, serde_json::to_vec(&pin).unwrap()).unwrap();
    // Invalid identity bytes deliberately prove preflight does not parse/unlock.
    std::fs::write(&identity_path, "synthetic identity placeholder; not a key").unwrap();
    std::fs::write(dir.path().join("config.toml"), format!(
        "tenant_id = 'team-blue'\ncapture_raw_values = true\nresolve_raw_values = true\nraw_manifest = '{}'\nraw_trust = '{}'\nraw_identity = '{}'\n",
        manifest_path.display(), trust_path.display(), identity_path.display()
    )).unwrap();
    command(dir.path())
        .args(["config", "--check"])
        .assert()
        .success()
        .stdout(is_empty());
    std::fs::write(&manifest_path, "{\"synthetic-invalid-policy-canary\":true}").unwrap();
    let output = command(dir.path())
        .args(["config", "--check"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-invalid-policy-canary"));
}

#[test]
fn setup_check_rejects_missing_enabled_paths_and_existing_incompatible_state() {
    for setting in ["capture_raw_values = true\n", "resolve_raw_values = true\n"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), setting).unwrap();
        command(dir.path())
            .args(["config", "--check"])
            .assert()
            .failure();
    }
    let dir = tempfile::tempdir().unwrap();
    let runtime = dir.path().join("tenants/personal/devices/local");
    std::fs::create_dir_all(&runtime).unwrap();
    let state = runtime.join("state.db");
    std::fs::write(&state, "synthetic incompatible database").unwrap();
    let saved = std::fs::read(&state).unwrap();
    command(dir.path())
        .args(["config", "--check"])
        .assert()
        .failure();
    assert_eq!(std::fs::read(state).unwrap(), saved);
}

#[test]
fn setup_check_accepts_metadata_only_without_reader_policy_or_token() {
    let dir = tempfile::tempdir().unwrap();
    command(dir.path())
        .args(["config", "--check"])
        .assert()
        .success()
        .stdout(is_empty());
}

#[test]
fn public_config_anchors_relative_state_to_the_original_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project with spaces");
    std::fs::create_dir(&project).unwrap();
    let output = command(dir.path())
        .current_dir(&project)
        .env("GENTLY_STATE_DIR", "state")
        .args(["config", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let config: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        config["state_dir"],
        project
            .canonicalize()
            .unwrap()
            .join("state")
            .to_string_lossy()
            .as_ref()
    );
}
