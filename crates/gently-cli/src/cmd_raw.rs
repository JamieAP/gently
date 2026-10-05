//! Explicit device enrollment and encrypted reader/owner-key setup.

use clap::Subcommand;
use gently_raw::{Manifest, OwnerKey, SignedManifest, TrustPin, VerifiedManifest};
use serde::de::DeserializeOwned;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const MAX_ENROLLMENT_BYTES: usize = 64 * 1024;

#[derive(Subcommand)]
pub enum RawCommand {
    /// Create a passphrase-encrypted software reader identity.
    Identity {
        #[arg(long)]
        out: PathBuf,
    },
    /// Create an owner signing key encrypted to enrolled reader recipients.
    OwnerKey {
        #[arg(long, required = true)]
        recipient: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Sign a recipient manifest with an explicitly unlocked owner key.
    Sign {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        owner_key: PathBuf,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Pin a verified owner and remember the newest locally enrolled epoch.
    Trust {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        owner_public: String,
        #[arg(long)]
        out: PathBuf,
    },
}

pub fn run(command: RawCommand) -> anyhow::Result<()> {
    match command {
        RawCommand::Identity { out } => identity(&out),
        RawCommand::OwnerKey { recipient, out } => owner_key(&recipient, &out),
        RawCommand::Sign {
            manifest,
            owner_key,
            identity,
            out,
        } => sign(&manifest, &owner_key, &identity, &out),
        RawCommand::Trust {
            manifest,
            owner_public,
            out,
        } => trust(&manifest, &owner_public, &out),
    }
}

fn identity(out: &Path) -> anyhow::Result<()> {
    ensure_new_target(out)?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("reader identity creation requires a private interactive terminal");
    }
    let passphrase = Zeroizing::new(rpassword::prompt_password("Reader passphrase: ")?);
    let confirm = Zeroizing::new(rpassword::prompt_password("Confirm reader passphrase: ")?);
    if *passphrase != *confirm {
        anyhow::bail!("reader passphrases do not match");
    }
    let (recipient, encrypted) = gently_raw::generate_encrypted_identity(&passphrase)?;
    atomic_write(out, &encrypted, false)?;
    println!("Recipient: {recipient}");
    Ok(())
}

fn owner_key(recipients: &[String], out: &Path) -> anyhow::Result<()> {
    ensure_new_target(out)?;
    let owner = OwnerKey::generate();
    let encrypted = gently_raw::encrypt_bytes(recipients, &owner.secret_bytes())?;
    atomic_write(out, encrypted.as_bytes(), false)?;
    println!("Owner public key: {}", owner.verification_key_b64());
    Ok(())
}

fn sign(
    manifest_path: &Path,
    owner_path: &Path,
    identity_path: &Path,
    out: &Path,
) -> anyhow::Result<()> {
    let manifest: Manifest = read_json(manifest_path)?;
    let encrypted = read_bounded(owner_path)?;
    let encrypted = std::str::from_utf8(&encrypted)
        .map_err(|_| anyhow::anyhow!("invalid encrypted owner key encoding"))?;
    gently_store::private_fs::harden_existing_file(identity_path)?;
    let identities = gently_raw::load_identities(identity_path)?;
    let private = gently_raw::decrypt_bytes(encrypted.trim(), &identities, 32)?;
    let owner = OwnerKey::from_secret_bytes(&private)?;
    let signed = gently_raw::sign_manifest(manifest, &owner)?;
    let candidate_pin = pin_for(&signed.manifest, owner.verification_key_b64())?;
    VerifiedManifest::verify(&signed, &candidate_pin)?;
    if target_exists(out)? {
        let previous: SignedManifest = read_json(out)?;
        let previous_pin = pin_for(&previous.manifest, owner.verification_key_b64())?;
        // Existing policy may have expired; signature verification at epoch zero
        // lets the same owner explicitly renew it without accepting a new root.
        VerifiedManifest::verify_at(&previous, &previous_pin, 0)?;
        VerifiedManifest::verify(&signed, &previous_pin)?;
    }
    atomic_write(out, &encode_json(&signed)?, true)
}

fn trust(manifest_path: &Path, owner_public: &str, out: &Path) -> anyhow::Result<()> {
    let signed: SignedManifest = read_json(manifest_path)?;
    let candidate_pin = pin_for(&signed.manifest, owner_public.to_owned())?;
    VerifiedManifest::verify(&signed, &candidate_pin)?;
    if target_exists(out)? {
        let previous: TrustPin = read_json(out)?;
        if previous.tenant_id != candidate_pin.tenant_id
            || previous.owner_verify_key_b64 != candidate_pin.owner_verify_key_b64
        {
            anyhow::bail!("existing trust pin has a different tenant or owner");
        }
        VerifiedManifest::verify(&signed, &previous)?;
    }
    atomic_write(out, &encode_json(&candidate_pin)?, true)
}

fn pin_for(manifest: &Manifest, owner_verify_key_b64: String) -> anyhow::Result<TrustPin> {
    Ok(TrustPin {
        tenant_id: manifest.tenant_id.clone(),
        owner_verify_key_b64,
        min_epoch: manifest.key_epoch,
        manifest_digest: gently_raw::manifest_digest(manifest)?,
    })
}

fn read_bounded(path: &Path) -> anyhow::Result<Vec<u8>> {
    gently_store::private_fs::harden_existing_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_ENROLLMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ENROLLMENT_BYTES {
        anyhow::bail!("enrollment input exceeds 64 KiB");
    }
    Ok(bytes)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&read_bounded(path)?)
        .map_err(|_| anyhow::anyhow!("invalid enrollment JSON"))
}

fn encode_json(value: &impl serde::Serialize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| anyhow::anyhow!("cannot encode public enrollment data"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_ENROLLMENT_BYTES {
        anyhow::bail!("enrollment output exceeds 64 KiB");
    }
    Ok(bytes)
}

fn target_exists(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            gently_store::private_fs::harden_existing_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn ensure_new_target(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => anyhow::bail!("output already exists; encrypted keys are never overwritten"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct TemporaryOutput(PathBuf);
impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> anyhow::Result<()> {
    if replace {
        target_exists(path)?;
    } else {
        ensure_new_target(path)?;
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize()?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("output must name a file"))?;
    let destination = parent.join(name);
    let temporary = TemporaryOutput(parent.join(format!(
        ".gently-enrollment-{}.tmp",
        gently_raw::new_raw_ref()
    )));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary.0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if replace {
        // Renaming replaces the pathname atomically, never follows a link to
        // truncate another file. Check owner and link safety before the swap.
        target_exists(&destination)?;
        fs::rename(&temporary.0, &destination)?;
    } else {
        // hard_link provides atomic create-if-absent on both target platforms.
        fs::hard_link(&temporary.0, &destination)?;
        fs::remove_file(&temporary.0)?;
    }
    File::open(&parent)?.sync_all()?;
    Ok(())
}
