//! Login UI has no credentials. Only a deliberate click opens the vault in a tty.
use crate::{cmd_service, config::Config};
use anyhow::{ensure, Context, Result};
use fs4::fs_std::FileExt;
use gently_store::private_fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LABEL: &str = "dev.gently.unlock-reminder";
const MARKER: &str = "# Gently managed unlock reminder";

fn home() -> Result<PathBuf> {
    dirs::home_dir().context("home directory unavailable")
}
fn agent_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}
fn app_path() -> Result<PathBuf> {
    Ok(home()?.join("Applications/Unlock Gently.app"))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
fn executable(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        private_fs::harden_existing_file(path)?;
        ensure!(
            std::fs::read_to_string(path)?.contains(MARKER),
            "refusing to overwrite an unmanaged launcher"
        );
    }
    private_fs::write_private_file(path, content)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn unlock_script(cfg: &Config, project: &Path, provider: &Path, gently: &Path) -> String {
    let q = |p: &Path| quote(&p.to_string_lossy());
    format!("#!/bin/zsh\n{MARKER}\nunset GENTLY_TOKEN GENTLY_READ_TOKEN\ncd -- {} || exit 1\n{} service start || exit 1\n{} run auto -- /usr/bin/env GENTLY_STATE_DIR={} GENTLY_COLLECTOR_URL={} GENTLY_TENANT_ID={} GENTLY_DEVICE_ID={} {} service unlock\nresult=$?\nif [ $result -ne 0 ]; then\n  printf '%s\\n' 'Gently is still locked. Close this window and try Unlock Gently again.'\n  read -r '?Press Return to close.'\nfi\nexit $result\n", q(project), q(gently), q(provider), q(&cfg.state_dir), quote(&cfg.collector_url), quote(&cfg.tenant_id), quote(&cfg.device_id), q(gently))
}

pub fn install(project: &Path) -> Result<()> {
    ensure!(cfg!(target_os = "macos"), "login reminders require macOS");
    let project = project
        .canonicalize()
        .context("project directory unavailable")?;
    ensure!(project.is_dir(), "project directory required");
    let cfg = Config::load()?;
    cfg.require_collector_url()?;
    cfg.ensure_state_dir()?;
    let provider = home()?.join(".local/bin/agent-secrets");
    ensure!(
        provider.is_file(),
        "install agent-secrets before adding the unlock reminder"
    );
    ensure!(
        Path::new("/Applications/Ghostty.app").is_dir(),
        "install Ghostty before adding the unlock reminder"
    );
    let gently = std::env::current_exe()?;
    let script = cfg.runtime_dir().join("unlock-service.command");
    executable(&script, &unlock_script(&cfg, &project, &provider, &gently))?;
    let app = app_path()?;
    let launcher = app.join("Contents/MacOS/unlock-gently");
    if app.exists() {
        ensure!(
            launcher.exists() && std::fs::read_to_string(&launcher)?.contains(MARKER),
            "refusing to replace an unmanaged application"
        );
    }
    for dir in [
        app.clone(),
        app.join("Contents"),
        app.join("Contents/MacOS"),
    ] {
        private_fs::ensure_private_dir(&dir)?;
    }
    let app_script = format!("#!/bin/sh\n{MARKER}\nunset GENTLY_TOKEN GENTLY_READ_TOKEN\nexec /usr/bin/open -na /Applications/Ghostty.app --args --title='Unlock Gently' -e /bin/zsh -f {}\n", quote(&script.to_string_lossy()));
    executable(&launcher, &app_script)?;
    private_fs::write_private_file(
        &app.join("Contents/Info.plist"),
        r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.gently.unlock</string><key>CFBundleName</key><string>Unlock Gently</string><key>CFBundleExecutable</key><string>unlock-gently</string><key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/></dict></plist>"#,
    )?;
    let path = agent_path()?;
    if path.exists() {
        private_fs::harden_existing_file(&path)?;
        ensure!(
            std::fs::read_to_string(&path)?.contains("<!-- Gently managed login service -->"),
            "unmanaged reminder registration"
        );
    }
    private_fs::write_private_file(
        &path,
        cmd_service::job_plist(&gently, &cfg, LABEL, "remind", false),
    )?;
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let target = format!("{domain}/{LABEL}");
    let _ = Command::new("/bin/launchctl")
        .env_clear()
        .args(["bootout", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    ensure!(
        Command::new("/bin/launchctl")
            .env_clear()
            .args(["bootstrap", &domain])
            .arg(path)
            .status()?
            .success(),
        "could not load login reminder"
    );
    println!("Login reminder enabled; repeats hourly while locked. Unlock Gently is available in your Applications folder.");
    Ok(())
}

pub fn remind() -> Result<()> {
    ensure!(cfg!(target_os = "macos"), "login reminders require macOS");
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let lock = private_fs::open_private_file(&cfg.runtime_dir().join("reminder.lock"), false)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let output = Command::new(std::env::current_exe()?)
        .args(["service", "status"])
        .env_clear()
        .env("HOME", home()?)
        .env("GENTLY_STATE_DIR", &cfg.state_dir)
        .env("GENTLY_COLLECTOR_URL", &cfg.collector_url)
        .env("GENTLY_TENANT_ID", &cfg.tenant_id)
        .env("GENTLY_DEVICE_ID", &cfg.device_id)
        .output()?;
    // Failed status means unavailable, never proof of an unlocked service.
    if output.status.success() {
        if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
            if status["locked"] == false {
                return Ok(());
            }
        }
    }
    let output = Command::new("/usr/bin/osascript").env_clear().args(["-e", r#"button returned of (display dialog "Gently is locked. Capture is queued locally; cloud export and queries need an unlock. You can also open Unlock Gently from your Applications folder later." with title "Unlock Gently" buttons {"Later", "Unlock Gently"} default button "Unlock Gently")"#]).output()?;
    if output.status.success() && output.stdout == b"Unlock Gently\n" {
        ensure!(
            Command::new("/usr/bin/open")
                .env_clear()
                .arg(app_path()?)
                .status()?
                .success(),
            "could not open Unlock Gently"
        );
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = agent_path()?;
    if !path.exists() {
        return Ok(());
    }
    private_fs::harden_existing_file(&path)?;
    ensure!(
        std::fs::read_to_string(&path)?.contains("<!-- Gently managed login service -->"),
        "unmanaged reminder registration"
    );
    let target = format!("gui/{}/{LABEL}", unsafe { libc::geteuid() });
    let _ = Command::new("/bin/launchctl")
        .env_clear()
        .args(["bootout", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    std::fs::remove_file(path)?;
    // Retain the explicitly clickable application; never recursively delete apps.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_launchers_never_embed_credentials_and_pin_public_routing() {
        let cfg = Config {
            collector_url: "https://collector.example".into(),
            token: "synthetic-ingest-value".into(),
            read_token: "synthetic-read-value".into(),
            state_dir: "/tmp/state with 'quotes'".into(),
            prefer_quic: false,
            outbox_cap: 1,
            export_batch: 1,
            export_timeout_secs: 1,
            query_timeout_secs: 1,
            capture_raw_values: true,
            tenant_id: "test".into(),
            device_id: "host".into(),
            sync_raw_values: true,
            resolve_raw_values: false,
            raw_manifest: None,
            raw_trust: None,
            raw_identity: None,
        };
        let script = unlock_script(
            &cfg,
            Path::new("/tmp/a project's directory"),
            Path::new("/tmp/vault helper"),
            Path::new("/tmp/gently"),
        );
        assert!(!script.contains(&cfg.token));
        assert!(!script.contains(&cfg.read_token));
        assert!(script.contains("run auto -- /usr/bin/env GENTLY_STATE_DIR="));
        assert!(script.contains("GENTLY_COLLECTOR_URL='https://collector.example'"));
        assert!(script.find("unset GENTLY_TOKEN").unwrap() < script.find("run auto").unwrap());
        let job = cmd_service::job_plist(Path::new("/tmp/gently"), &cfg, LABEL, "remind", false);
        assert!(job.contains("<key>StartInterval</key><integer>3600</integer>"));
        assert!(!job.contains("KeepAlive"));
        assert!(!job.contains(&cfg.token));
        assert!(!job.contains(&cfg.read_token));
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            let mut check = Command::new("/bin/zsh")
                .args(["-f", "-n"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            check
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            assert!(check.wait().unwrap().success());
        }
    }
    #[test]
    fn shell_arguments_are_single_words() {
        assert_eq!(quote("a'b;$HOME"), "'a'\"'\"'b;$HOME'");
    }
}
