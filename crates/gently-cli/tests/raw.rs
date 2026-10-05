//! Enrollment tests use generated fixture keys in scratch directories only.

use assert_cmd::Command;
use gently_raw::{
    DeviceIdentity, Manifest, OwnerKey, ReaderIdentities, Recipient, SignedManifest, TrustPin,
};
use std::path::Path;

fn command() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("gently"))
}

fn unsigned(reader: &DeviceIdentity, epoch: u64) -> Manifest {
    Manifest {
        version: 1,
        tenant_id: "personal".into(),
        key_epoch: epoch,
        expires_unix_secs: 9_007_199_254_740_991,
        readers: vec![Recipient {
            device_id: "synthetic-reader".into(),
            key_id: format!("reader-{epoch}"),
            recipient: reader.to_public().to_string(),
        }],
    }
}

fn signed_file(dir: &Path, owner: &OwnerKey, epoch: u64) -> std::path::PathBuf {
    let manifest =
        gently_raw::sign_manifest(unsigned(&DeviceIdentity::generate(), epoch), owner).unwrap();
    let path = dir.join(format!("manifest-{epoch}.json"));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    path
}

fn trust_command(manifest: &Path, owner: &OwnerKey, out: &Path) -> Command {
    let mut command = command();
    command
        .args(["raw", "trust", "--manifest"])
        .arg(manifest)
        .arg("--owner-public")
        .arg(owner.verification_key_b64())
        .arg("--out")
        .arg(out);
    command
}

#[test]
fn local_trust_updates_same_owner_epoch_and_rejects_rollback_and_owner_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let owner = OwnerKey::generate();
    let first = signed_file(dir.path(), &owner, 1);
    let second = signed_file(dir.path(), &owner, 2);
    let out = dir.path().join("trust.json");
    trust_command(&first, &owner, &out).assert().success();
    trust_command(&second, &owner, &out).assert().success();
    let saved = std::fs::read(&out).unwrap();
    let pin: TrustPin = serde_json::from_slice(&saved).unwrap();
    assert_eq!(pin.min_epoch, 2);
    let fork = signed_file(dir.path(), &owner, 2);
    trust_command(&fork, &owner, &out).assert().failure();
    assert_eq!(std::fs::read(&out).unwrap(), saved);
    trust_command(&first, &owner, &out).assert().failure();
    assert_eq!(std::fs::read(&out).unwrap(), saved);
    let stranger = OwnerKey::generate();
    let replacement = signed_file(dir.path(), &stranger, 3);
    trust_command(&replacement, &stranger, &out)
        .assert()
        .failure();
    assert_eq!(std::fs::read(&out).unwrap(), saved);
}

#[test]
fn tampered_or_wrong_owner_policy_never_creates_trust_output() {
    let dir = tempfile::tempdir().unwrap();
    let owner = OwnerKey::generate();
    let manifest = signed_file(dir.path(), &owner, 1);
    let out = dir.path().join("trust.json");
    let stranger = OwnerKey::generate();
    trust_command(&manifest, &stranger, &out).assert().failure();
    assert!(!out.exists());
    let mut tampered: SignedManifest =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    tampered.manifest.readers[0].recipient = DeviceIdentity::generate().to_public().to_string();
    std::fs::write(&manifest, serde_json::to_vec(&tampered).unwrap()).unwrap();
    trust_command(&manifest, &owner, &out).assert().failure();
    assert!(!out.exists());
}

#[test]
fn owner_key_creation_persists_only_ciphertext_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let reader = DeviceIdentity::generate();
    let out = dir.path().join("owner.age");
    let result = command()
        .args(["raw", "owner-key", "--recipient"])
        .arg(reader.to_public().to_string())
        .arg("--out")
        .arg(&out)
        .assert()
        .success();
    let stdout = String::from_utf8(result.get_output().stdout.clone()).unwrap();
    assert!(stdout.starts_with("Owner public key: "));
    assert!(!stdout.contains("AGE-SECRET-KEY"));
    let encrypted = std::fs::read_to_string(&out).unwrap();
    let identities = ReaderIdentities::from_native(vec![reader.clone()]);
    let private = gently_raw::decrypt_bytes(encrypted.trim(), &identities, 32).unwrap();
    let owner = OwnerKey::from_secret_bytes(&private).unwrap();
    assert_eq!(
        stdout.trim(),
        format!("Owner public key: {}", owner.verification_key_b64())
    );
    command()
        .args(["raw", "owner-key", "--recipient"])
        .arg(reader.to_public().to_string())
        .arg("--out")
        .arg(&out)
        .assert()
        .failure();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), encrypted);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn identity_creation_requires_a_private_interactive_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("reader.age");
    command()
        .args(["raw", "identity", "--out"])
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains("private interactive terminal"));
    assert!(!out.exists());
}

#[test]
fn explicit_owner_unlock_signs_a_valid_manifest_without_printing_key_material() {
    use age_fixture::write_reader;
    let dir = tempfile::tempdir().unwrap();
    let reader = DeviceIdentity::generate();
    let reader_path = write_reader(dir.path(), &reader);
    let owner = OwnerKey::generate();
    let owner_path = dir.path().join("owner.age");
    std::fs::write(
        &owner_path,
        gently_raw::encrypt_bytes(&[reader.to_public().to_string()], &owner.secret_bytes())
            .unwrap(),
    )
    .unwrap();
    let template = dir.path().join("unsigned.json");
    std::fs::write(
        &template,
        serde_json::to_vec(&unsigned(&reader, 1)).unwrap(),
    )
    .unwrap();
    let out = dir.path().join("signed.json");
    command()
        .args(["raw", "sign", "--manifest"])
        .arg(&template)
        .arg("--owner-key")
        .arg(&owner_path)
        .arg("--identity")
        .arg(&reader_path)
        .arg("--out")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
    let signed: SignedManifest = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let pin = TrustPin {
        tenant_id: "personal".into(),
        owner_verify_key_b64: owner.verification_key_b64(),
        min_epoch: 1,
        manifest_digest: gently_raw::manifest_digest(&signed.manifest).unwrap(),
    };
    gently_raw::VerifiedManifest::verify(&signed, &pin).unwrap();
}

#[test]
fn enrollment_json_limits_and_link_checks_preserve_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let owner = OwnerKey::generate();
    let manifest = dir.path().join("too-large.json");
    std::fs::write(&manifest, vec![b'x'; 64 * 1024 + 1]).unwrap();
    let out = dir.path().join("trust.json");
    trust_command(&manifest, &owner, &out).assert().failure();
    assert!(!out.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let valid = signed_file(dir.path(), &owner, 1);
        let target = dir.path().join("original.json");
        std::fs::write(&target, "original fixture").unwrap();
        symlink(&target, &out).unwrap();
        trust_command(&valid, &owner, &out).assert().failure();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original fixture"
        );
        std::fs::remove_file(&out).unwrap();
        std::fs::hard_link(&target, &out).unwrap();
        trust_command(&valid, &owner, &out).assert().failure();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original fixture"
        );
    }
}

mod age_fixture {
    use super::*;
    pub fn write_reader(dir: &Path, reader: &DeviceIdentity) -> std::path::PathBuf {
        // A generated synthetic identity is confined to this disposable fixture.
        let path = dir.join("synthetic-reader.agekey");
        let secret = reader.to_string();
        use age::secrecy::ExposeSecret;
        std::fs::write(&path, secret.expose_secret()).unwrap();
        path
    }
}

#[cfg(unix)]
#[test]
fn protected_software_reader_never_runs_inherited_pinentry_program() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (recipient, reader) =
        gently_raw::generate_encrypted_identity("synthetic reader passphrase").unwrap();
    let reader_path = dir.path().join("reader.age");
    std::fs::write(&reader_path, reader).unwrap();
    let owner = OwnerKey::generate();
    let owner_path = dir.path().join("owner.age");
    std::fs::write(
        &owner_path,
        gently_raw::encrypt_bytes(&[recipient], &owner.secret_bytes()).unwrap(),
    )
    .unwrap();
    let manifest = dir.path().join("unsigned.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&unsigned(&DeviceIdentity::generate(), 1)).unwrap(),
    )
    .unwrap();
    let pinentry = dir.path().join("synthetic-pinentry");
    std::fs::write(&pinentry, "#!/bin/sh\n: > pinentry-started\nexit 1\n").unwrap();
    std::fs::set_permissions(&pinentry, std::fs::Permissions::from_mode(0o700)).unwrap();
    let out = dir.path().join("signed.json");
    command()
        .current_dir(dir.path())
        .env("PINENTRY_PROGRAM", &pinentry)
        .args(["raw", "sign", "--manifest"])
        .arg(&manifest)
        .arg("--owner-key")
        .arg(&owner_path)
        .arg("--identity")
        .arg(&reader_path)
        .arg("--out")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "could not unlock reader identity",
        ));
    assert!(!dir.path().join("pinentry-started").exists());
    assert!(!out.exists());
}

#[cfg(unix)]
#[test]
fn immediate_private_terminal_input_is_hidden_and_terminal_state_is_restored() {
    let output = std::process::Command::new("python3")
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin")
        .args(["-c", include_str!("private_reader_tty.py")])
        .arg(assert_cmd::cargo::cargo_bin!("gently"))
        .output()
        .expect("Python is required for the synthetic private-terminal regression");
    assert!(
        output.status.success(),
        "private-terminal regression failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
