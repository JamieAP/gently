use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

fn readers() -> (Vec<DeviceIdentity>, Manifest, TrustPin, OwnerKey) {
    let identities = vec![DeviceIdentity::generate(), DeviceIdentity::generate()];
    let owner = OwnerKey::from_secret_bytes(&[7; 32]).unwrap();
    let manifest = Manifest {
        version: VERSION,
        tenant_id: "personal".into(),
        key_epoch: 2,
        expires_unix_secs: 9_007_199_254_740_991,
        readers: identities
            .iter()
            .enumerate()
            .map(|(i, identity)| Recipient {
                device_id: format!("device-{i}"),
                key_id: format!("reader-{i}"),
                recipient: identity.to_public().to_string(),
            })
            .collect(),
    };
    let pin = TrustPin {
        tenant_id: "personal".into(),
        owner_verify_key_b64: owner.verification_key_b64(),
        min_epoch: 2,
        manifest_digest: manifest_digest(&manifest).unwrap(),
    };
    (identities, manifest, pin, owner)
}

fn context() -> RawContext {
    RawContext {
        tenant_id: "personal".into(),
        device_id: "capture-linux".into(),
        key_epoch: 2,
        raw_ref: new_raw_ref(),
        session_id: "session-1".into(),
        harness: "codex".into(),
        event: "UserPromptSubmit".into(),
    }
}
fn payload_fields() -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    (
        BTreeMap::from([("gently.prompt".into(), "private fixture content".into())]),
        BTreeMap::from([("gently.prompt".into(), vec!["0123456789abcdef".into()])]),
    )
}
fn object() -> (RawObject, Vec<DeviceIdentity>, VerifiedManifest) {
    let (ids, manifest, pin, owner) = readers();
    let verified =
        VerifiedManifest::verify(&sign_manifest(manifest, &owner).unwrap(), &pin).unwrap();
    let (fields, bindings) = payload_fields();
    let object = seal(&verified, context(), fields, bindings).unwrap();
    (object, ids, verified)
}

#[test]
fn owner_signed_manifest_enrolls_public_readers() {
    let (_, manifest, pin, owner) = readers();
    let signed = sign_manifest(manifest.clone(), &owner).unwrap();
    assert_eq!(
        VerifiedManifest::verify(&signed, &pin).unwrap().manifest(),
        &manifest
    );
}

#[test]
fn signed_manifest_rejects_recipient_substitution_wrong_owner_tenant_and_rollback() {
    let (_, manifest, pin, owner) = readers();
    let signed = sign_manifest(manifest, &owner).unwrap();
    let mut altered = signed.clone();
    altered.manifest.readers[0].recipient = DeviceIdentity::generate().to_public().to_string();
    assert!(VerifiedManifest::verify(&altered, &pin).is_err());
    let wrong_owner = TrustPin {
        owner_verify_key_b64: OwnerKey::generate().verification_key_b64(),
        ..pin.clone()
    };
    assert!(VerifiedManifest::verify(&signed, &wrong_owner).is_err());
    let wrong_tenant = TrustPin {
        tenant_id: "another".into(),
        ..pin.clone()
    };
    assert!(VerifiedManifest::verify(&signed, &wrong_tenant).is_err());
    let rollback = TrustPin {
        min_epoch: 3,
        ..pin
    };
    assert!(VerifiedManifest::verify(&signed, &rollback).is_err());
}

#[test]
fn manifest_rejects_empty_duplicate_and_unsupported_readers() {
    let (_, manifest, _, owner) = readers();
    let mut candidate = manifest;
    candidate.readers.push(candidate.readers[0].clone());
    assert!(sign_manifest(candidate, &owner).is_err());
    let (_, mut manifest, _, owner) = readers();
    manifest.readers.clear();
    assert!(sign_manifest(manifest, &owner).is_err());
    let (_, mut manifest, _, owner) = readers();
    manifest.readers[0].recipient = "age1se1untrustedpluginrecipient".into();
    assert!(sign_manifest(manifest, &owner).is_err());
}

#[test]
fn manifest_expiry_requires_recent_local_policy_refresh() {
    let (_, mut manifest, mut pin, owner) = readers();
    manifest.expires_unix_secs = 100;
    pin.manifest_digest = manifest_digest(&manifest).unwrap();
    let signed = sign_manifest(manifest, &owner).unwrap();
    assert!(VerifiedManifest::verify_at(&signed, &pin, 99).is_ok());
    assert!(VerifiedManifest::verify_at(&signed, &pin, 100).is_err());
    assert!(VerifiedManifest::verify_at(&signed, &pin, 101).is_err());
}

#[test]
fn software_reader_identity_is_created_encrypted_and_unlocks_explicitly() {
    let (recipient, encrypted) =
        generate_encrypted_identity("synthetic test passphrase only").unwrap();
    assert!(encrypted.starts_with(b"age-encryption.org/v1\n"));
    assert!(!String::from_utf8_lossy(&encrypted).contains("AGE-SECRET-KEY-"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.identity.age");
    std::fs::write(&path, encrypted).unwrap();
    assert!(load_identities_with_passphrase(&path, "wrong synthetic passphrase").is_err());
    let identities =
        load_identities_with_passphrase(&path, "synthetic test passphrase only").unwrap();
    let ciphertext = encrypt_bytes(&[recipient], b"synthetic content").unwrap();
    assert_eq!(
        &**decrypt_bytes(&ciphertext, &identities, 100).unwrap(),
        b"synthetic content"
    );
}

#[test]
fn native_public_only_capture_can_be_opened_by_either_enrolled_reader() {
    let (object, ids, _) = object();
    object.validate().unwrap();
    let encoded = serde_json::to_string(&object).unwrap();
    assert!(!encoded.contains("private fixture content"));
    for id in ids {
        let identities = ReaderIdentities::from_native(vec![id]);
        let payload = open(&object, &object.context, &identities).unwrap();
        assert_eq!(payload.fields["gently.prompt"], "private fixture content");
        assert_eq!(payload.bindings["gently.prompt"], ["0123456789abcdef"]);
    }
}

#[test]
fn raw_refs_and_equal_content_ciphertexts_are_random() {
    let (object, _, manifest) = object();
    let (fields, bindings) = payload_fields();
    let second = seal(&manifest, object.context.clone(), fields, bindings).unwrap();
    assert_ne!(second.ciphertext_b64, object.ciphertext_b64);
    let reference = new_raw_ref();
    assert_eq!(reference.len(), 32);
    assert!(reference
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_ne!(reference, new_raw_ref());
}

#[test]
fn decrypt_rejects_wrong_reader_tampered_ciphertext_and_swapped_context() {
    let (object, ids, _) = object();
    let identities = ReaderIdentities::from_native(vec![ids.into_iter().next().unwrap()]);
    let stranger = ReaderIdentities::from_native(vec![DeviceIdentity::generate()]);
    assert!(open(&object, &object.context, &stranger).is_err());
    let mut tampered = object.clone();
    let mut bytes = STANDARD.decode(&tampered.ciphertext_b64).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    tampered.ciphertext_b64 = STANDARD.encode(bytes);
    assert!(open(&tampered, &tampered.context, &identities).is_err());
    let mut swapped = object.clone();
    swapped.context.session_id = "other-session".into();
    assert!(open(&swapped, &swapped.context, &identities).is_err());
    let mut different_field_context = object.context.clone();
    different_field_context.raw_ref = new_raw_ref();
    assert!(open(&object, &different_field_context, &identities).is_err());
}

#[test]
fn object_rejects_plaintext_malformed_data_unknown_fields_and_excess_size() {
    let (mut object, _, _) = object();
    object.ciphertext_b64 = STANDARD.encode(b"private fixture content");
    assert!(object.validate().is_err());
    object.ciphertext_b64 = "invalid base64".into();
    assert!(object.validate().is_err());
    object.ciphertext_b64 = STANDARD.encode(vec![0; MAX_CIPHERTEXT_BYTES + 1]);
    assert!(object.validate().is_err());
    let mut json = serde_json::to_value(&object).unwrap();
    json["plaintext"] = serde_json::Value::String("fixture".into());
    assert!(serde_json::from_value::<RawObject>(json).is_err());
}

#[test]
fn ciphertext_decrypt_limit_and_field_bindings_are_enforced() {
    let (object, ids, manifest) = object();
    let identities = ReaderIdentities::from_native(vec![ids.into_iter().next().unwrap()]);
    assert!(matches!(
        decrypt_bytes(&object.ciphertext_b64, &identities, 1),
        Err(Error::Oversized("decrypted data"))
    ));
    let (fields, mut bindings) = payload_fields();
    bindings.insert("gently.assistant".into(), vec!["span".into()]);
    assert!(seal(&manifest, context(), fields, bindings).is_err());
    let fields = BTreeMap::from([("gently.prompt".into(), "x".repeat(MAX_PLAINTEXT_BYTES + 1))]);
    let bindings = BTreeMap::from([("gently.prompt".into(), vec!["span".into()])]);
    assert!(matches!(
        seal(&manifest, context(), fields, bindings),
        Err(Error::Oversized("plaintext"))
    ));
    assert_eq!(
        Error::Oversized("plaintext").to_string(),
        "invalid raw encryption data: plaintext exceeds size limit"
    );
}

#[test]
fn owner_key_round_trips_only_inside_age_ciphertext() {
    let id = DeviceIdentity::generate();
    let identities = ReaderIdentities::from_native(vec![id.clone()]);
    let key = OwnerKey::generate();
    let encrypted = encrypt_bytes(&[id.to_public().to_string()], &key.secret_bytes()).unwrap();
    let decrypted = decrypt_bytes(&encrypted, &identities, 32).unwrap();
    let restored = OwnerKey::from_secret_bytes(&decrypted).unwrap();
    assert_eq!(restored.verification_key_b64(), key.verification_key_b64());
}

#[test]
fn explicit_reader_loads_identity_file_without_capture_key_access() {
    use age::secrecy::ExposeSecret;
    let dir = tempfile::tempdir().unwrap();
    let id = DeviceIdentity::generate();
    let path = dir.path().join("synthetic-reader.agekey");
    std::fs::write(&path, id.to_string().expose_secret()).unwrap();
    let identities = load_identities(&path).unwrap();
    let encrypted = encrypt_bytes(&[id.to_public().to_string()], b"fixture").unwrap();
    assert_eq!(
        &**decrypt_bytes(&encrypted, &identities, 100).unwrap(),
        b"fixture"
    );
}

#[test]
fn mac_hardware_tag_recipient_encrypts_natively_without_plugin_access() {
    // Public recipient from age's native tag interoperability fixture. No
    // identity, plugin executable or hardware interaction is used in capture.
    let recipient = "age1tag1qt8lw0ual6avlwmwatk888yqnmdamm7xfd0wak53ut6elz5c4swx2yqdj4e";
    let cipher = encrypt_bytes(&[recipient.into()], b"public-only fixture").unwrap();
    let object = RawObject {
        version: VERSION,
        context: context(),
        ciphertext_b64: cipher,
    };
    object.validate().unwrap();
}

#[test]
fn encrypted_armored_identity_unlocks_with_explicit_reader_callback() {
    let (recipient, encrypted) =
        generate_encrypted_identity("synthetic armor test passphrase").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.identity.age.asc");
    let mut armored =
        age::armor::ArmoredWriter::wrap_output(Vec::new(), age::armor::Format::AsciiArmor).unwrap();
    use std::io::Write;
    armored.write_all(&encrypted).unwrap();
    let armored = armored.finish().unwrap();
    std::fs::write(&path, armored).unwrap();
    let identities =
        load_identities_with_passphrase(&path, "synthetic armor test passphrase").unwrap();
    let ciphertext = encrypt_bytes(&[recipient], b"fixture").unwrap();
    assert_eq!(
        &**decrypt_bytes(&ciphertext, &identities, 100).unwrap(),
        b"fixture"
    );
}

#[test]
fn tenant_device_key_ids_and_epochs_match_cloud_transport_bounds() {
    let (_, manifest, _, owner) = readers();
    for id in ["with.dot".to_owned(), "x".repeat(65), "../escape".into()] {
        let mut candidate = manifest.clone();
        candidate.tenant_id = id.clone();
        assert!(sign_manifest(candidate, &owner).is_err());
        let mut candidate = manifest.clone();
        candidate.readers[0].key_id = id;
        assert!(sign_manifest(candidate, &owner).is_err());
    }
    let mut too_large = manifest;
    too_large.key_epoch = 9_007_199_254_740_992;
    assert!(sign_manifest(too_large, &owner).is_err());
    let (mut object, _, _) = object();
    object.context.device_id = "unsafe.device".into();
    assert!(object.validate().is_err());
}

#[test]
fn trusted_executable_selection_rejects_repository_and_relative_paths() {
    assert!(!trusted_binary_path(Path::new("./age")));
    assert!(!trusted_binary_path(Path::new("/tmp/repository/age")));
    assert!(trusted_binary_path(Path::new(
        "/opt/homebrew/Cellar/age/1.3.1/bin/age"
    )));
    assert!(trusted_binary_path(Path::new("/usr/bin/age")));
}

#[cfg(unix)]
#[test]
fn external_reader_is_bounded_concurrent_and_sanitizes_process_environment() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("synthetic-age");
    // This fixture fails if inherited AGEDEBUG enters the child and emits more
    // than a pipe buffer before consuming its input to detect write/read stalls.
    std::fs::write(&script, "#!/bin/sh\n[ -z \"$AGEDEBUG\" ] || exit 3\n[ \"$PATH\" = '/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin' ] || exit 4\n/usr/bin/head -c 131072 /dev/zero\n/bin/cat >/dev/null\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let external = ExternalReader {
        executable: script,
        identity_path: dir.path().join("synthetic.identity"),
    };
    let result = decrypt_external(
        &external,
        &vec![0; 256 * 1024],
        200 * 1024,
        std::time::Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(result.len(), 131072);
    assert!(decrypt_external(&external, b"fixture", 5, std::time::Duration::from_secs(2)).is_err());
}

#[cfg(unix)]
#[test]
fn external_reader_timeout_kills_stalled_process_without_returning_diagnostics() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("synthetic-age");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'synthetic-sensitive-diagnostic' >&2\nexec /bin/sleep 20\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let external = ExternalReader {
        executable: script,
        identity_path: dir.path().join("synthetic.identity"),
    };
    let start = std::time::Instant::now();
    let error = decrypt_external(
        &external,
        b"fixture",
        100,
        std::time::Duration::from_millis(100),
    )
    .err()
    .unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert!(error.to_string().contains("timed out"));
    assert!(!error.to_string().contains("synthetic-sensitive-diagnostic"));
}

#[cfg(unix)]
#[test]
fn external_reader_deadline_covers_descendant_pipes_after_parent_exits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("synthetic-age");
    std::fs::write(&script, "#!/bin/sh\n/bin/sleep 2 &\nexec /usr/bin/true\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let external = ExternalReader {
        executable: script,
        identity_path: dir.path().join("synthetic.identity"),
    };
    let start = std::time::Instant::now();
    let result = decrypt_external(
        &external,
        b"fixture",
        100,
        std::time::Duration::from_millis(300),
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    assert!(result.err().unwrap().to_string().contains("timed out"));
}

#[test]
fn encrypted_plugin_identity_is_rejected_before_native_reader_conversion() {
    use std::io::Write;
    let identity = age::plugin::Identity::default_for_plugin("se").unwrap();
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        "synthetic plugin identity passphrase",
    ));
    let mut encrypted = Vec::new();
    let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
    writer.write_all(identity.to_string().as_bytes()).unwrap();
    writer.finish().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic-plugin.identity.age");
    std::fs::write(&path, encrypted).unwrap();
    let result = load_identities_with_passphrase(&path, "synthetic plugin identity passphrase");
    assert!(result.is_err());
    // This contains a default plugin reference, never a real hardware identity.
}

#[test]
fn locally_pinned_policy_rejects_owner_signed_same_epoch_forks() {
    let (_, manifest, pin, owner) = readers();
    let mut fork = manifest.clone();
    fork.readers[0].recipient = DeviceIdentity::generate().to_public().to_string();
    let signed = sign_manifest(fork.clone(), &owner).unwrap();
    assert!(VerifiedManifest::verify(&signed, &pin).is_err());
    fork.key_epoch += 1;
    assert!(VerifiedManifest::verify(&sign_manifest(fork, &owner).unwrap(), &pin).is_ok());
    assert_eq!(manifest_digest(&manifest).unwrap().len(), 64);
}
