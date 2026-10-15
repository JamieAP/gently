//! A credential-free login controller. Unlock is explicit, local and ephemeral.
use crate::{config::Config, query_broker};
use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use fs4::fs_std::FileExt;
use gently_store::private_fs;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use zeroize::{Zeroize, Zeroizing};

const LABEL: &str = "dev.gently.service";
const MAX_MESSAGE: u64 = 16 * 1024;

#[derive(Subcommand)]
pub enum ServiceCommand {
    /// Install this executable as a macOS login service, initially locked.
    Install,
    /// Add a macOS login reminder and an Unlock Gently application.
    InstallReminder {
        /// Working directory for the configured unlock command.
        #[arg(long)]
        project_dir: PathBuf,
        /// Executable wrapper that obtains credentials and runs service unlock.
        #[arg(long)]
        unlock_command: PathBuf,
        /// Optional executable accepting the generated script path as one argument.
        /// Otherwise macOS opens the .command file with its associated terminal.
        #[arg(long)]
        terminal_command: Option<PathBuf>,
    },
    #[command(hide = true)]
    Remind,
    /// Start the installed login service (does not unlock it).
    Start,
    /// Stop the service and its children; queued history is preserved.
    Stop,
    /// Remove the login registration, never captured data.
    Uninstall,
    /// Show controller and child process state as JSON.
    Status,
    /// Hand inherited ingest/read credentials to the private service socket.
    Unlock,
    /// Stop children and forget the in-memory credentials.
    Lock,
    #[command(hide = true)]
    Run,
    #[command(hide = true)]
    Broker,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    collector_url: String,
    tenant: String,
    device: String,
    #[serde(default)]
    ingest: String,
    #[serde(default)]
    read: String,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.ingest.zeroize();
        self.read.zeroize();
    }
}

pub fn run(command: ServiceCommand) -> Result<()> {
    disable_core_dumps()?;
    match command {
        ServiceCommand::Install => install(),
        ServiceCommand::InstallReminder {
            project_dir,
            unlock_command,
            terminal_command,
        } => crate::service_reminder::install(
            &project_dir,
            &unlock_command,
            terminal_command.as_deref(),
        ),
        ServiceCommand::Remind => crate::service_reminder::remind(),
        ServiceCommand::Start => launch("start"),
        ServiceCommand::Stop => launch("stop"),
        ServiceCommand::Uninstall => {
            crate::service_reminder::uninstall()?;
            launch("stop")?;
            std::fs::remove_file(plist_path()?)?;
            println!("Removed login registration; captured data is unchanged.");
            Ok(())
        }
        command => {
            let mut cfg = Config::load()?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            match command {
                ServiceCommand::Run => {
                    cfg.token.zeroize();
                    cfg.read_token.zeroize();
                    std::env::remove_var("GENTLY_TOKEN");
                    std::env::remove_var("GENTLY_READ_TOKEN");
                    // No descendants inherit the controller's launch environment.
                    disable_core_dumps()?;
                    rt.block_on(controller(&cfg))
                }
                ServiceCommand::Broker => {
                    ensure!(
                        !cfg.read_token.is_empty() && cfg.token.is_empty(),
                        "broker requires only a read credential"
                    );
                    disable_core_dumps()?;
                    cfg.ensure_state_dir()?;
                    let _lock = lock(&cfg.runtime_dir().join("query.lock"))?;
                    rt.block_on(async {
                        let broker = query_broker::QueryBroker::bind(&cfg)?;
                        let mut term = tokio::signal::unix::signal(
                            tokio::signal::unix::SignalKind::terminate(),
                        )?;
                        tokio::select! {
                            result = broker.serve(&cfg) => result,
                            _ = term.recv() => Ok(()),
                            _ = tokio::signal::ctrl_c() => Ok(()),
                        }
                    })
                }
                ServiceCommand::Status => rt.block_on(client(&cfg, "status")),
                ServiceCommand::Unlock => rt.block_on(client(&cfg, "unlock")),
                ServiceCommand::Lock => rt.block_on(client(&cfg, "lock")),
                _ => unreachable!(),
            }
        }
    }
}

fn disable_core_dumps() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    ensure!(
        unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } == 0,
        "cannot disable core dumps"
    );
    Ok(())
}

fn lock(path: &Path) -> Result<std::fs::File> {
    let file = private_fs::open_private_file(path, false)?;
    file.try_lock_exclusive()
        .context("another service process holds the lock")?;
    Ok(file)
}

async fn receive(stream: &mut UnixStream) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    tokio::time::timeout(
        Duration::from_secs(2),
        stream.take(MAX_MESSAGE + 1).read_to_end(&mut bytes),
    )
    .await??;
    ensure!(
        bytes.len() <= MAX_MESSAGE as usize,
        "service message too large"
    );
    Ok(bytes)
}

fn same_user(stream: &UnixStream) -> Result<()> {
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "service peer is not the current user"
    );
    Ok(())
}

async fn client(cfg: &Config, operation: &str) -> Result<()> {
    let path = cfg.runtime_dir().join("service.sock");
    query_broker::validate_socket(&path)?;
    let mut stream = UnixStream::connect(path)
        .await
        .context("service is not running")?;
    same_user(&stream)?;
    let mut request = Request {
        operation: operation.into(),
        collector_url: cfg.collector_url.clone(),
        tenant: cfg.tenant_id.clone(),
        device: cfg.device_id.clone(),
        ingest: String::new(),
        read: String::new(),
    };
    if operation == "unlock" {
        ensure!(
            !cfg.token.is_empty() && !cfg.read_token.is_empty() && cfg.token != cfg.read_token,
            "unlock requires distinct inherited GENTLY_TOKEN and GENTLY_READ_TOKEN"
        );
        request.ingest.clone_from(&cfg.token);
        request.read.clone_from(&cfg.read_token);
    }
    let bytes = Zeroizing::new(serde_json::to_vec(&request)?);
    ensure!(
        bytes.len() <= MAX_MESSAGE as usize,
        "service message too large"
    );
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    let bytes = receive(&mut stream).await?;
    let response: serde_json::Value =
        serde_json::from_slice(&bytes).context("invalid service response")?;
    if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        // Never echo arbitrary socket response text into a terminal.
        let reason = match response.get("error").and_then(|v| v.as_str()) {
            Some("configuration_mismatch") => "collector or namespace configuration mismatch",
            Some("invalid_request") => "invalid request",
            Some("credentials_required") => "distinct credentials required",
            Some("message_too_large") => "message too large",
            _ => "request rejected",
        };
        anyhow::bail!("service: {reason}");
    }
    println!("{response}");
    Ok(())
}

struct SocketGuard {
    path: PathBuf,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

struct Worker {
    child: Option<Child>,
    next: Instant,
    backoff: u64,
    restarts: u64,
    last_exit: Option<i32>,
}
impl Worker {
    fn new() -> Self {
        Self {
            child: None,
            next: Instant::now(),
            backoff: 1,
            restarts: 0,
            last_exit: None,
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Both children handle SIGINT. Allow cleanup before a bounded
            // fallback kill; an unavailable collector must not prevent lock.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGINT);
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn tick(&mut self, cfg: &Config, secret: &str, broker: bool) -> Result<()> {
        if let Some(child) = &mut self.child {
            let Some(status) = child.try_wait()? else {
                return Ok(());
            };
            self.last_exit = status.code();
            self.restarts += 1;
            self.child = None;
            self.next = Instant::now() + Duration::from_secs(self.backoff);
            self.backoff = (self.backoff * 2).min(30);
        }
        if Instant::now() >= self.next {
            match child_command(cfg, secret, broker)?.spawn() {
                Ok(child) => self.child = Some(child),
                Err(_) => {
                    self.restarts += 1;
                    self.next = Instant::now() + Duration::from_secs(self.backoff);
                    self.backoff = (self.backoff * 2).min(30);
                }
            }
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn child_command(cfg: &Config, secret: &str, broker: bool) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.env_clear();
    if let Some(home) = dirs::home_dir() {
        command.env("HOME", home);
    }
    command
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("GENTLY_STATE_DIR", &cfg.state_dir)
        .env("GENTLY_COLLECTOR_URL", &cfg.collector_url)
        .env("GENTLY_TENANT_ID", &cfg.tenant_id)
        .env("GENTLY_DEVICE_ID", &cfg.device_id)
        .env(
            "GENTLY_CAPTURE_RAW_VALUES",
            cfg.capture_raw_values.to_string(),
        )
        .env("GENTLY_SYNC_RAW_VALUES", cfg.sync_raw_values.to_string())
        .env("GENTLY_RESOLVE_RAW_VALUES", "false")
        .env(
            if broker {
                "GENTLY_READ_TOKEN"
            } else {
                "GENTLY_TOKEN"
            },
            secret,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, path) in [
        ("GENTLY_RAW_MANIFEST", &cfg.raw_manifest),
        ("GENTLY_RAW_TRUST", &cfg.raw_trust),
    ] {
        if let Some(path) = path {
            command.env(name, path);
        }
    }
    if broker {
        command.args(["service", "broker"]);
    } else {
        command.args(["export", "--watch"]);
    }
    Ok(command)
}

async fn controller(cfg: &Config) -> Result<()> {
    cfg.require_collector_url()?;
    cfg.ensure_state_dir()?;
    let _lock = lock(&cfg.runtime_dir().join("service.lock"))?;
    let path = cfg.runtime_dir().join("service.sock");
    if std::fs::symlink_metadata(&path).is_ok() {
        query_broker::validate_socket(&path)?;
        match UnixStream::connect(&path).await {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(&path)?
            }
            _ => anyhow::bail!("service socket is already in use"),
        }
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let _socket = SocketGuard {
        inode: std::fs::symlink_metadata(&path)?.ino(),
        path,
    };
    let mut credentials: Option<Request> = None;
    let mut exporter = Worker::new();
    let mut broker = Worker::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = tick.tick() => {
                if let Some(keys) = &credentials {
                    exporter.tick(cfg, &keys.ingest, false)?;
                    broker.tick(cfg, &keys.read, true)?;
                }
            }
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                let result: Result<()> = async {
                    same_user(&stream)?;
                    let bytes = receive(&mut stream).await?;
                    // Never attach serde errors: malformed input can contain secrets.
                    let request: Request = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid service request"))?;
                    ensure!(request.collector_url == cfg.collector_url && request.tenant == cfg.tenant_id && request.device == cfg.device_id, "service configuration mismatch");
                    match request.operation.as_str() {
                        "status" => {
                            ensure!(request.ingest.is_empty() && request.read.is_empty(), "unexpected credentials");
                            Ok(())
                        }
                        "lock" => {
                            ensure!(request.ingest.is_empty() && request.read.is_empty(), "unexpected credentials");
                            exporter.stop(); broker.stop(); credentials = None; Ok(())
                        }
                        "unlock" => {
                            ensure!(!request.ingest.is_empty() && !request.read.is_empty() && request.ingest != request.read, "distinct credentials required");
                            exporter.stop(); broker.stop();
                            exporter = Worker::new(); broker = Worker::new();
                            credentials = Some(request); Ok(())
                        }
                        _ => anyhow::bail!("unknown operation"),
                    }
                }.await;
                let error = result.as_ref().err().map(|error| match error.to_string().as_str() {
                    "service configuration mismatch" => "configuration_mismatch",
                    "invalid service request" => "invalid_request",
                    "distinct credentials required" => "credentials_required",
                    "service message too large" => "message_too_large",
                    _ => "request_rejected",
                });
                let response = json!({"ok": result.is_ok(), "error": error, "locked": credentials.is_none(),
                    "controller_pid": std::process::id(),
                    "exporter_pid": exporter.child.as_ref().map(Child::id),
                    "broker_pid": broker.child.as_ref().map(Child::id),
                    "exporter_restarts": exporter.restarts, "broker_restarts": broker.restarts,
                    "exporter_last_exit": exporter.last_exit, "broker_last_exit": broker.last_exit});
                let bytes = serde_json::to_vec(&response)?;
                let _ = tokio::time::timeout(Duration::from_secs(2), stream.write_all(&bytes)).await;
            }
        }
    }
    Ok(())
}

fn plist_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("home directory unavailable")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub(crate) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn plist(executable: &Path, cfg: &Config) -> String {
    job_plist(executable, cfg, LABEL, "run", true)
}

pub(crate) fn job_plist(
    executable: &Path,
    cfg: &Config,
    label: &str,
    action: &str,
    keep_alive: bool,
) -> String {
    let executable = xml(&executable.to_string_lossy());
    let state = xml(&cfg.state_dir.to_string_lossy());
    let url = xml(&cfg.collector_url);
    let tenant = xml(&cfg.tenant_id);
    let device = xml(&cfg.device_id);
    let scheduling = if keep_alive {
        "<key>KeepAlive</key><true/>"
    } else {
        "<key>StartInterval</key><integer>3600</integer>"
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Gently managed login service -->
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>service</string><string>{action}</string></array>
<key>RunAtLoad</key><true/>{scheduling}
<key>ThrottleInterval</key><integer>10</integer>
<key>Umask</key><integer>63</integer>
<key>EnvironmentVariables</key><dict>
<key>GENTLY_STATE_DIR</key><string>{state}</string>
<key>GENTLY_COLLECTOR_URL</key><string>{url}</string>
<key>GENTLY_TENANT_ID</key><string>{tenant}</string>
<key>GENTLY_DEVICE_ID</key><string>{device}</string>
</dict>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
<key>SoftResourceLimits</key><dict><key>Core</key><integer>0</integer></dict>
<key>HardResourceLimits</key><dict><key>Core</key><integer>0</integer></dict>
</dict></plist>
"#
    )
}

fn install() -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "login installation currently requires macOS; use service run elsewhere"
    );
    let cfg = Config::load()?;
    cfg.require_collector_url()?;
    cfg.ensure_state_dir()?;
    let path = plist_path()?;
    let parent = path.parent().context("missing LaunchAgents directory")?;
    // Do not chmod the user's shared LaunchAgents directory.
    std::fs::create_dir_all(parent)?;
    ensure!(
        !std::fs::symlink_metadata(parent)?.file_type().is_symlink(),
        "LaunchAgents cannot be a symlink"
    );
    if path.exists() {
        private_fs::harden_existing_file(&path)?;
        ensure!(
            std::fs::read_to_string(&path)?.contains("<!-- Gently managed login service -->"),
            "refusing to replace an unmanaged registration"
        );
    }
    private_fs::write_private_file(&path, plist(&std::env::current_exe()?, &cfg))?;
    println!("Installed locked-at-login service. Run gently service start, then unlock through your secret provider.");
    Ok(())
}

fn launch(action: &str) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "login service control currently requires macOS"
    );
    let path = plist_path()?;
    private_fs::harden_existing_file(&path)?;
    ensure!(
        std::fs::read_to_string(&path)?.contains("<!-- Gently managed login service -->"),
        "not a managed registration"
    );
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let target = format!("{domain}/{LABEL}");
    let registered = Command::new("/bin/launchctl")
        .env_clear()
        .args(["print", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();
    if (action == "start" && registered) || (action == "stop" && !registered) {
        return Ok(());
    }
    let mut command = Command::new("/bin/launchctl");
    if action == "start" {
        command.args(["bootstrap", &domain]).arg(path);
    } else {
        command.args(["bootout", &target]);
    }
    // Never forward vault credentials to launchctl.
    command
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ensure!(
        command.status()?.success(),
        "launchctl service operation failed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Config {
        Config {
            collector_url: "https://collector.example".into(),
            token: "synthetic-ingest-fixture".into(),
            read_token: "synthetic-reader-fixture".into(),
            state_dir: PathBuf::from("/tmp/gently-service-fixture"),
            prefer_quic: false,
            outbox_cap: 1,
            export_batch: 1,
            export_timeout_secs: 1,
            query_timeout_secs: 1,
            capture_raw_values: true,
            sync_raw_values: true,
            resolve_raw_values: false,
            tenant_id: "test".into(),
            device_id: "host".into(),
            raw_manifest: None,
            raw_trust: None,
            raw_identity: None,
        }
    }
    #[test]
    fn login_configuration_contains_only_public_settings() {
        let cfg = fixture();
        let text = plist(Path::new("/tmp/a&b/gently"), &cfg);
        assert!(text.contains("/tmp/a&amp;b/gently"));
        assert!(!text.contains(&cfg.token));
        assert!(!text.contains(&cfg.read_token));
        assert!(!text.contains("GENTLY_TOKEN"));
        assert!(!text.contains("GENTLY_READ_TOKEN"));
        assert!(text.contains("<key>KeepAlive</key><true/>"));
    }
    #[test]
    fn child_environments_separate_capabilities() {
        let cfg = fixture();
        for broker in [false, true] {
            let secret = if broker { &cfg.read_token } else { &cfg.token };
            let command = child_command(&cfg, secret, broker).unwrap();
            let vars: std::collections::BTreeMap<_, _> = command.get_envs().collect();
            let own = if broker {
                "GENTLY_READ_TOKEN"
            } else {
                "GENTLY_TOKEN"
            };
            let other = if broker {
                "GENTLY_TOKEN"
            } else {
                "GENTLY_READ_TOKEN"
            };
            assert_eq!(
                vars.get(std::ffi::OsStr::new(own)).copied().flatten(),
                Some(std::ffi::OsStr::new(secret))
            );
            assert!(!vars.contains_key(std::ffi::OsStr::new(other)));
            assert!(!vars.contains_key(std::ffi::OsStr::new("OPENAI_API_KEY")));
            assert!(!vars.contains_key(std::ffi::OsStr::new("GENTLY_RAW_IDENTITY")));
            assert!(!command.get_args().any(|arg| arg == secret.as_str()));
        }
    }
    #[test]
    fn xml_escapes_all_special_characters() {
        assert_eq!(xml("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
    }
    #[test]
    fn request_is_strict_and_never_debug_formatted() {
        assert!(serde_json::from_str::<Request>(
            r#"{"operation":"status","collector_url":"x","tenant":"t","device":"d","unknown":true}"#
        )
        .is_err());
    }
}
