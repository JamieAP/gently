//! Device-only age encryption and locally pinned recipient policy.
//!
//! Captures use public X25519 and native tag recipients only. They never invoke
//! plugins or access reader identities. Identity access is restricted to the
//! explicit reader/owner commands. Cloud storage is not a trust root: manifests
//! must verify against a locally enrolled owner key and remembered epoch.
//!
//! Age authenticates ciphertext and the inner event/field bindings. It does not
//! prove which capture host created an object; recipient public keys are public,
//! and a malicious server can fabricate or replay entire objects. Authentication
//! and authorization remain separate from this confidentiality boundary.

use age::secrecy::{ExposeSecret, SecretString};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

pub const VERSION: u32 = 1;
pub const MAX_PLAINTEXT_BYTES: usize = 256 * 1024;
pub const MAX_CIPHERTEXT_BYTES: usize = 512 * 1024;
const MAX_IDENTITY_BYTES: usize = 64 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const READER_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
const MANIFEST_DOMAIN: &[u8] = b"gently.recipient-manifest.v1\0";
pub struct ReaderIdentities {
    native: Vec<Box<dyn age::Identity + Send + Sync>>,
    external: Option<ExternalReader>,
}
impl ReaderIdentities {
    pub fn from_native(identities: Vec<DeviceIdentity>) -> Self {
        Self {
            native: identities.into_iter().map(|id| Box::new(id) as _).collect(),
            external: None,
        }
    }
}
struct ExternalReader {
    executable: PathBuf,
    identity_path: PathBuf,
}
fn trusted_binary_path(path: &Path) -> bool {
    use std::path::Component;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    if !matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("age" | "rage" | "age-plugin-se")
    ) {
        return false;
    }
    READER_PATH
        .split(':')
        .any(|directory| path.parent() == Some(Path::new(directory)))
        || path.starts_with("/opt/homebrew/Cellar")
        || path.starts_with("/usr/local/Cellar")
}

fn trusted_program(names: &[&str]) -> Result<PathBuf> {
    for name in names {
        for directory in READER_PATH.split(':') {
            let candidate = Path::new(directory).join(name);
            if candidate.is_file() {
                let canonical = candidate.canonicalize()?;
                if !trusted_binary_path(&canonical) {
                    return Err(Error::Invalid(
                        "age reader or plugin resolves outside trusted installation directories",
                    ));
                }
                return Ok(canonical);
            }
        }
    }
    Err(Error::Invalid(
        "install age or rage and age-plugin-se in a system or Homebrew bin directory",
    ))
}

fn decrypt_external(
    external: &ExternalReader,
    ciphertext: &[u8],
    max_bytes: usize,
    timeout: Duration,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut command = Command::new(&external.executable);
    command
        .args(["--decrypt", "--identity"])
        .arg(&external.identity_path)
        .env_clear()
        .env("PATH", READER_PATH)
        .env("LC_ALL", "C")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The hardware plugin uses its own native user-presence dialog. No secret
    // environment or AGEDEBUG protocol logging is forwarded to the child.
    if let Some(home) = std::env::var_os("HOME") {
        if Path::new(&home).is_absolute() {
            command.env("HOME", home);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| Error::Crypto("could not start trusted age reader"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or(Error::Crypto("reader input pipe unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or(Error::Crypto("reader output pipe unavailable"))?;
    let ciphertext = ciphertext.to_vec();
    let writer = std::thread::spawn(move || input.write_all(&ciphertext));
    let overflow = Arc::new(AtomicBool::new(false));
    let overflow_signal = Arc::clone(&overflow);
    let reader = std::thread::spawn(move || {
        let mut plaintext = Zeroizing::new(Vec::new());
        output
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut plaintext)?;
        if plaintext.len() > max_bytes {
            overflow_signal.store(true, Ordering::Release);
        }
        Ok::<_, std::io::Error>(plaintext)
    });
    let deadline = Instant::now() + timeout;
    let mut parent_status = None;
    let status = loop {
        if overflow.load(Ordering::Acquire) {
            kill_reader(&mut child);
            break Err(Error::Invalid("decrypted data exceeds size limit"));
        }
        if parent_status.is_none() {
            match child.try_wait() {
                Ok(status) => parent_status = status,
                Err(_) => {
                    kill_reader(&mut child);
                    break Err(Error::Crypto("trusted age reader failed"));
                }
            }
        }
        // A plugin or other descendant can hold either pipe after the CLI has
        // exited. Keep the deadline active until the threads have both stopped.
        if writer.is_finished() && reader.is_finished() {
            if let Some(status) = parent_status {
                break Ok(status);
            }
        }
        if Instant::now() >= deadline {
            kill_reader(&mut child);
            break Err(Error::Crypto("trusted age reader timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let write_result = writer
        .join()
        .map_err(|_| Error::Crypto("reader input failed"));
    let read_result = reader
        .join()
        .map_err(|_| Error::Crypto("reader output failed"));
    if !status?.success() {
        return Err(Error::Crypto(
            "trusted age reader could not decrypt ciphertext",
        ));
    }
    write_result?.map_err(|_| Error::Crypto("reader input failed"))?;
    let plaintext = read_result?.map_err(|_| Error::Crypto("reader output failed"))?;
    if plaintext.len() > max_bytes {
        return Err(Error::Invalid("decrypted data exceeds size limit"));
    }
    Ok(plaintext)
}

fn kill_reader(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // Child was placed in its own process group before exec. Stop the CLI,
        // plugin and any pinentry child together so pipes cannot outlive timeout.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
pub use age::x25519::Identity as DeviceIdentity;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid raw encryption data: {0}")]
    Invalid(&'static str),
    #[error("raw encryption operation failed: {0}")]
    Crypto(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recipient {
    pub device_id: String,
    pub key_id: String,
    pub recipient: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub tenant_id: String,
    pub key_epoch: u64,
    pub expires_unix_secs: u64,
    pub readers: Vec<Recipient>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    pub manifest: Manifest,
    pub signature_b64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustPin {
    pub tenant_id: String,
    pub owner_verify_key_b64: String,
    pub min_epoch: u64,
    pub manifest_digest: String,
}

pub fn manifest_digest(manifest: &Manifest) -> Result<String> {
    Ok(blake3::hash(&manifest_signing_bytes(manifest)?)
        .to_hex()
        .to_string())
}

pub struct VerifiedManifest(Manifest);
impl VerifiedManifest {
    pub fn verify(signed: &SignedManifest, pin: &TrustPin) -> Result<Self> {
        Self::verify_at(signed, pin, now_unix_secs()?)
    }
    pub fn verify_at(signed: &SignedManifest, pin: &TrustPin, now_unix_secs: u64) -> Result<Self> {
        validate_manifest(&signed.manifest)?;
        if pin.tenant_id != signed.manifest.tenant_id {
            return Err(Error::Invalid(
                "manifest tenant differs from local trust pin",
            ));
        }
        if pin.min_epoch == 0 || signed.manifest.key_epoch < pin.min_epoch {
            return Err(Error::Invalid(
                "manifest epoch is older than local trust pin",
            ));
        }
        if pin.manifest_digest.len() != 64
            || !pin
                .manifest_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid(
                "local trust pin has an invalid manifest digest",
            ));
        }
        if signed.manifest.key_epoch == pin.min_epoch
            && manifest_digest(&signed.manifest)? != pin.manifest_digest
        {
            return Err(Error::Invalid(
                "same-epoch manifest differs from locally enrolled policy",
            ));
        }
        if signed.manifest.expires_unix_secs <= now_unix_secs {
            return Err(Error::Invalid(
                "recipient manifest expired; enroll refreshed policy locally",
            ));
        }
        let key_bytes = STANDARD
            .decode(&pin.owner_verify_key_b64)
            .map_err(|_| Error::Invalid("invalid owner verification key"))?;
        let key_array = key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("invalid owner verification key"))?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(key_array)
            .map_err(|_| Error::Invalid("invalid owner verification key"))?;
        let signature_bytes = STANDARD
            .decode(&signed.signature_b64)
            .map_err(|_| Error::Invalid("invalid manifest signature"))?;
        let signature = ed25519_dalek::Signature::from_slice(&signature_bytes)
            .map_err(|_| Error::Invalid("invalid manifest signature"))?;
        key.verify_strict(&manifest_signing_bytes(&signed.manifest)?, &signature)
            .map_err(|_| Error::Invalid("manifest signature does not match pinned owner"))?;
        Ok(Self(signed.manifest.clone()))
    }
    pub fn manifest(&self) -> &Manifest {
        &self.0
    }
}

pub struct OwnerKey(ed25519_dalek::SigningKey);
impl OwnerKey {
    pub fn generate() -> Self {
        Self(ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng))
    }
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self> {
        let bytes: &[u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Invalid("invalid owner key length"))?;
        Ok(Self(ed25519_dalek::SigningKey::from_bytes(bytes)))
    }
    /// For immediate in-memory encryption only; never persist the returned bytes.
    pub fn secret_bytes(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(self.0.to_bytes().to_vec())
    }
    pub fn verification_key_b64(&self) -> String {
        STANDARD.encode(self.0.verifying_key().as_bytes())
    }
}
pub fn sign_manifest(manifest: Manifest, key: &OwnerKey) -> Result<SignedManifest> {
    validate_manifest(&manifest)?;
    let signature = key.0.sign(&manifest_signing_bytes(&manifest)?);
    Ok(SignedManifest {
        manifest,
        signature_b64: STANDARD.encode(signature.to_bytes()),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RawContext {
    pub tenant_id: String,
    pub device_id: String,
    pub key_epoch: u64,
    pub raw_ref: String,
    pub session_id: String,
    pub harness: String,
    pub event: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RawObject {
    pub version: u32,
    pub context: RawContext,
    pub ciphertext_b64: String,
}
impl RawObject {
    /// Check the public envelope and complete age header without a private key.
    /// Payload authentication requires `open` on an explicitly enrolled reader.
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::Invalid("unsupported raw object version"));
        }
        validate_context(&self.context)?;
        let bytes = decode_ciphertext(&self.ciphertext_b64)?;
        let decryptor = age::Decryptor::new(bytes.as_slice())
            .map_err(|_| Error::Invalid("invalid age ciphertext header"))?;
        if decryptor.is_scrypt() {
            return Err(Error::Invalid(
                "raw objects require enrolled public recipients",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RawPayload {
    pub version: u32,
    pub context: RawContext,
    pub fields: BTreeMap<String, String>,
    pub bindings: BTreeMap<String, Vec<String>>,
}

pub fn new_raw_ref() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn seal(
    manifest: &VerifiedManifest,
    context: RawContext,
    fields: BTreeMap<String, String>,
    bindings: BTreeMap<String, Vec<String>>,
) -> Result<RawObject> {
    validate_context(&context)?;
    if context.tenant_id != manifest.0.tenant_id || context.key_epoch != manifest.0.key_epoch {
        return Err(Error::Invalid(
            "capture context does not match recipient manifest",
        ));
    }
    if manifest.0.expires_unix_secs <= now_unix_secs()? {
        return Err(Error::Invalid(
            "recipient manifest expired; enroll refreshed policy locally",
        ));
    }
    let payload = RawPayload {
        version: VERSION,
        context: context.clone(),
        fields,
        bindings,
    };
    validate_payload(&payload)?;
    let bytes = Zeroizing::new(
        serde_json::to_vec(&payload)
            .map_err(|_| Error::Invalid("raw payload cannot be encoded"))?,
    );
    let recipients: Vec<String> = manifest
        .0
        .readers
        .iter()
        .map(|r| r.recipient.clone())
        .collect();
    let object = RawObject {
        version: VERSION,
        context,
        ciphertext_b64: encrypt_bytes(&recipients, &bytes)?,
    };
    object.validate()?;
    Ok(object)
}

pub fn open(
    object: &RawObject,
    expected: &RawContext,
    identities: &ReaderIdentities,
) -> Result<RawPayload> {
    object.validate()?;
    validate_context(expected)?;
    if &object.context != expected {
        return Err(Error::Invalid(
            "raw object does not match requested event context",
        ));
    }
    let bytes = decrypt_bytes(&object.ciphertext_b64, identities, MAX_PLAINTEXT_BYTES)?;
    let payload: RawPayload = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Invalid("decrypted raw payload is invalid"))?;
    validate_payload(&payload)?;
    if &payload.context != expected {
        return Err(Error::Invalid(
            "authenticated raw context does not match requested event",
        ));
    }
    Ok(payload)
}

/// Encrypt small secrets immediately in memory. Recipient parsing is native:
/// capture and key-generation paths never execute a plugin or prompt for keys.
pub fn encrypt_bytes(recipients: &[String], plaintext: &[u8]) -> Result<String> {
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(Error::Invalid("plaintext exceeds size limit"));
    }
    let recipients = native_recipients(recipients)?;
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| &**r as &dyn age::Recipient))
            .map_err(|_| Error::Crypto("could not wrap age file key"))?;
    let mut bytes = Vec::new();
    let mut writer = encryptor.wrap_output(&mut bytes)?;
    writer.write_all(plaintext)?;
    writer.finish()?;
    if bytes.len() > MAX_CIPHERTEXT_BYTES {
        return Err(Error::Invalid("ciphertext exceeds size limit"));
    }
    Ok(STANDARD.encode(bytes))
}

pub fn decrypt_bytes(
    ciphertext_b64: &str,
    identities: &ReaderIdentities,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    if max_bytes > MAX_PLAINTEXT_BYTES {
        return Err(Error::Invalid("decryption limit exceeds supported maximum"));
    }
    let bytes = decode_ciphertext(ciphertext_b64)?;
    let decryptor = age::Decryptor::new(bytes.as_slice())
        .map_err(|_| Error::Crypto("invalid age ciphertext"))?;
    if decryptor.is_scrypt() {
        return Err(Error::Invalid(
            "encrypted data requires enrolled public recipients",
        ));
    }
    if let Some(external) = &identities.external {
        return decrypt_external(external, &bytes, max_bytes, Duration::from_secs(60));
    }
    let reader = decryptor
        .decrypt(identities.native.iter().map(|i| &**i as &dyn age::Identity))
        .map_err(|_| Error::Crypto("reader could not decrypt ciphertext"))?;
    let mut output = Zeroizing::new(Vec::new());
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|_| Error::Crypto("ciphertext authentication failed"))?;
    if output.len() > max_bytes {
        return Err(Error::Invalid("decrypted data exceeds size limit"));
    }
    Ok(output)
}

/// Explicit reader-only operation. Software identities require a private
/// terminal; no PINENTRY_PROGRAM or inherited-PATH program is used. Secure
/// Enclave identities use a trusted age child
/// with a clean environment and the plugin's normal
/// biometric dialog. Returned identities stay in this reader process's memory.
pub fn load_identities(path: &Path) -> Result<ReaderIdentities> {
    load_identities_with_callbacks(path, PrivateTerminalCallbacks)
}

/// Dependency injection for explicit callers and synthetic tests, never hooks.
pub fn load_identities_with_passphrase(path: &Path, passphrase: &str) -> Result<ReaderIdentities> {
    load_identities_with_callbacks(
        path,
        PassphraseCallbacks(Arc::new(SecretString::from(passphrase))),
    )
}

pub fn generate_encrypted_identity(passphrase: &str) -> Result<(String, Vec<u8>)> {
    if passphrase.is_empty() {
        return Err(Error::Invalid("reader passphrase must not be empty"));
    }
    let identity = DeviceIdentity::generate();
    let recipient = identity.to_public().to_string();
    let plaintext = identity.to_string();
    let encryptor = age::Encryptor::with_user_passphrase(SecretString::from(passphrase));
    let mut encrypted = Vec::new();
    let mut writer = encryptor.wrap_output(&mut encrypted)?;
    writer.write_all(plaintext.expose_secret().as_bytes())?;
    writer.finish()?;
    Ok((recipient, encrypted))
}

fn load_identities_with_callbacks<C: age::Callbacks>(
    path: &Path,
    callbacks: C,
) -> Result<ReaderIdentities> {
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(path)?
        .take(MAX_IDENTITY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_IDENTITY_BYTES {
        return Err(Error::Invalid("reader identity exceeds size limit"));
    }
    let file = if bytes.starts_with(b"age-encryption.org/v1\n")
        || bytes.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----")
    {
        let reader = age::armor::ArmoredReader::new(bytes.as_slice());
        let decryptor = age::Decryptor::new(reader)
            .map_err(|_| Error::Crypto("invalid encrypted reader identity"))?;
        let encrypted = age::encrypted::EncryptedIdentity::new(decryptor, callbacks, Some(22))
            .ok_or(Error::Invalid(
                "encrypted reader identity must use a passphrase",
            ))?;
        encrypted
            .decrypt(path.to_str())
            .map_err(|_| Error::Crypto("could not unlock reader identity"))?
    } else {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Invalid("invalid reader identity encoding"))?;
        let mut native: Vec<Box<dyn age::Identity + Send + Sync>> = Vec::new();
        let mut hardware = false;
        for line in text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            if let Ok(identity) = line.parse::<DeviceIdentity>() {
                native.push(Box::new(identity));
            } else if let Ok(identity) = line.parse::<age::plugin::Identity>() {
                if identity.plugin() != "se" {
                    return Err(Error::Invalid(
                        "only enrolled Secure Enclave plugins are supported",
                    ));
                }
                hardware = true;
            } else {
                return Err(Error::Invalid("reader file contains non-identity data"));
            }
        }
        if hardware {
            let executable = trusted_program(&["age", "rage"])?;
            trusted_program(&["age-plugin-se"])?;
            return Ok(ReaderIdentities {
                native: Vec::new(),
                external: Some(ExternalReader {
                    executable,
                    identity_path: path.canonicalize()?,
                }),
            });
        }
        if native.is_empty() {
            return Err(Error::Invalid("reader identity file is empty"));
        }
        return Ok(ReaderIdentities {
            native,
            external: None,
        });
    };
    // This API only converts native software identities after passphrase unlock.
    // Plugins never enter age's inherited-PATH subprocess implementation.
    file.write_recipients_file(std::io::sink()).map_err(|_| {
        Error::Invalid("encrypted software identity file must contain native reader keys")
    })?;
    let identities = file
        .into_identities()
        .map_err(|_| Error::Crypto("could not load reader identity plugin"))?;
    if identities.is_empty() {
        return Err(Error::Invalid("reader identity file is empty"));
    }
    Ok(ReaderIdentities {
        native: identities,
        external: None,
    })
}

#[derive(Clone)]
struct PassphraseCallbacks(Arc<SecretString>);
impl age::Callbacks for PassphraseCallbacks {
    fn display_message(&self, _: &str) {}
    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        None
    }
    fn request_public_string(&self, _: &str) -> Option<String> {
        None
    }
    fn request_passphrase(&self, _: &str) -> Option<SecretString> {
        Some(SecretString::from(self.0.expose_secret()))
    }
}

#[derive(Clone, Copy)]
struct PrivateTerminalCallbacks;
impl age::Callbacks for PrivateTerminalCallbacks {
    fn display_message(&self, _: &str) {}
    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        None
    }
    fn request_public_string(&self, _: &str) -> Option<String> {
        None
    }
    fn request_passphrase(&self, _: &str) -> Option<SecretString> {
        // rpassword reads /dev/tty independently of stdin. A software reader may
        // be an explicitly launched stdio process with an attached terminal.
        let has_private_terminal = std::io::stdin().is_terminal()
            || std::fs::File::open("/dev/tty").is_ok_and(|tty| tty.is_terminal());
        if !has_private_terminal {
            return None;
        }
        let passphrase = Zeroizing::new(rpassword::prompt_password("Reader passphrase: ").ok()?);
        if passphrase.is_empty() {
            return None;
        }
        Some(SecretString::from(passphrase.as_str()))
    }
}

fn native_recipients(values: &[String]) -> Result<Vec<Box<dyn age::Recipient>>> {
    if values.is_empty() || values.len() > 64 {
        return Err(Error::Invalid("recipient count must be between 1 and 64"));
    }
    let mut seen = HashSet::new();
    values
        .iter()
        .map(|value| {
            if !seen.insert(value) {
                return Err(Error::Invalid("duplicate recipient"));
            }
            if let Ok(recipient) = value.parse::<age::x25519::Recipient>() {
                return Ok(Box::new(recipient) as Box<dyn age::Recipient>);
            }
            if let Ok(recipient) = value.parse::<age::tag::Recipient>() {
                return Ok(Box::new(recipient) as Box<dyn age::Recipient>);
            }
            Err(Error::Invalid(
                "reader recipient must be native age X25519 or tag",
            ))
        })
        .collect()
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.version != VERSION {
        return Err(Error::Invalid("unsupported manifest version"));
    }
    validate_id(&manifest.tenant_id)?;
    if manifest.key_epoch == 0
        || manifest.key_epoch > MAX_SAFE_INTEGER
        || manifest.expires_unix_secs == 0
        || manifest.expires_unix_secs > MAX_SAFE_INTEGER
    {
        return Err(Error::Invalid(
            "manifest epoch and expiry must be positive safe integers",
        ));
    }
    let mut devices = HashSet::new();
    let mut keys = HashSet::new();
    for reader in &manifest.readers {
        validate_id(&reader.device_id)?;
        validate_id(&reader.key_id)?;
        if !devices.insert(&reader.device_id) || !keys.insert(&reader.key_id) {
            return Err(Error::Invalid("duplicate reader device or key ID"));
        }
    }
    native_recipients(
        &manifest
            .readers
            .iter()
            .map(|r| r.recipient.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

fn manifest_signing_bytes(manifest: &Manifest) -> Result<Vec<u8>> {
    let mut bytes = MANIFEST_DOMAIN.to_vec();
    bytes.extend(
        serde_json::to_vec(manifest).map_err(|_| Error::Invalid("manifest cannot be encoded"))?,
    );
    Ok(bytes)
}

fn validate_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Invalid(
            "identifier must contain 1-64 ASCII letters, digits, dashes or underscores",
        ));
    }
    Ok(())
}

fn validate_context(context: &RawContext) -> Result<()> {
    validate_id(&context.tenant_id)?;
    validate_id(&context.device_id)?;
    if context.key_epoch == 0 || context.key_epoch > MAX_SAFE_INTEGER {
        return Err(Error::Invalid(
            "raw context epoch must be a positive safe integer",
        ));
    }
    if context.raw_ref.len() != 32
        || !context
            .raw_ref
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid(
            "raw reference must be 32 lowercase hexadecimal characters",
        ));
    }
    for value in [&context.session_id, &context.harness, &context.event] {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(Error::Invalid("invalid event context"));
        }
    }
    Ok(())
}

fn validate_payload(payload: &RawPayload) -> Result<()> {
    if payload.version != VERSION {
        return Err(Error::Invalid("unsupported raw payload version"));
    }
    validate_context(&payload.context)?;
    if payload.fields.is_empty() || payload.fields.len() > 64 || payload.bindings.len() > 64 {
        return Err(Error::Invalid("invalid raw field count"));
    }
    for field in payload.fields.keys() {
        if field.is_empty()
            || field.len() > 128
            || !field
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(Error::Invalid("invalid raw field name"));
        }
    }
    // Some captured fields belong only to the encrypted debug event, so bindings
    // may be a subset. A binding can never name a field absent from that event.
    for (field, spans) in &payload.bindings {
        if !payload.fields.contains_key(field) || spans.is_empty() || spans.len() > 256 {
            return Err(Error::Invalid("invalid raw field binding"));
        }
        let mut seen = HashSet::new();
        for span in spans {
            validate_id(span)?;
            if !seen.insert(span) {
                return Err(Error::Invalid("duplicate raw span binding"));
            }
        }
    }
    Ok(())
}

fn decode_ciphertext(ciphertext_b64: &str) -> Result<Vec<u8>> {
    if ciphertext_b64.len() > MAX_CIPHERTEXT_BYTES.div_ceil(3) * 4 {
        return Err(Error::Invalid("ciphertext exceeds size limit"));
    }
    let bytes = STANDARD
        .decode(ciphertext_b64)
        .map_err(|_| Error::Invalid("invalid ciphertext base64"))?;
    if bytes.len() > MAX_CIPHERTEXT_BYTES
        || bytes.len() < 64
        || !bytes.starts_with(b"age-encryption.org/v1\n")
    {
        return Err(Error::Invalid("invalid bounded age ciphertext"));
    }
    Ok(bytes)
}

fn now_unix_secs() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| Error::Invalid("system clock precedes Unix epoch"))
}

#[cfg(test)]
mod tests;
