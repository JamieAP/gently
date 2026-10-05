//! A controlling-terminal prompt whose input is private before it is visible.
use crate::{Error, Result};
use zeroize::Zeroizing;

/// Read a reader passphrase from the controlling terminal, never stdin or env.
/// Terminal flags are restored on normal return, read failure and cancellation.
pub fn prompt_reader_passphrase(prompt: &str) -> Result<Zeroizing<String>> {
    #[cfg(unix)]
    {
        unix::prompt(prompt)
    }
    #[cfg(not(unix))]
    {
        let _ = prompt;
        Err(Error::Invalid(
            "private reader terminals require Mac or Linux",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static PROMPT_LOCK: Mutex<()> = Mutex::new(());
    static PENDING_SIGNALS: AtomicU32 = AtomicU32::new(0);

    fn signal_bit(signal: libc::c_int) -> u32 {
        match signal {
            libc::SIGINT => 1,
            libc::SIGTERM => 2,
            libc::SIGHUP => 4,
            libc::SIGQUIT => 8,
            _ => 0,
        }
    }

    extern "C" fn note_signal(signal: libc::c_int) {
        // AtomicU32 is lock-free on supported Mac/Linux targets. No terminal
        // calls, allocation, locks or other non-signal-safe work happens here.
        PENDING_SIGNALS.fetch_or(signal_bit(signal), Ordering::Relaxed);
    }

    struct PromptSignals {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }
    impl PromptSignals {
        fn install() -> Result<Self> {
            let lock = PROMPT_LOCK
                .lock()
                .map_err(|_| Error::Crypto("private reader prompt is unavailable"))?;
            PENDING_SIGNALS.store(0, Ordering::Relaxed);
            let mut guard = Self {
                _lock: lock,
                previous: Vec::new(),
            };
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                let mut existing = MaybeUninit::<libc::sigaction>::uninit();
                if unsafe { libc::sigaction(signal, std::ptr::null(), existing.as_mut_ptr()) } != 0
                {
                    return Err(Error::Crypto(
                        "cannot inspect private reader terminal signals",
                    ));
                }
                if unsafe { existing.assume_init() }.sa_sigaction == libc::SIG_IGN {
                    // An inherited ignored action (e.g. nohup) stays ignored;
                    // it must not unexpectedly turn into prompt cancellation.
                    continue;
                }
                let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
                action.sa_sigaction = note_signal as *const () as usize;
                unsafe {
                    libc::sigemptyset(&mut action.sa_mask);
                }
                let mut previous = MaybeUninit::<libc::sigaction>::uninit();
                if unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) } != 0 {
                    return Err(Error::Crypto(
                        "cannot guard private reader terminal signals",
                    ));
                }
                guard
                    .previous
                    .push((signal, unsafe { previous.assume_init() }));
            }
            Ok(guard)
        }
    }
    impl Drop for PromptSignals {
        fn drop(&mut self) {
            // Declared before the terminal guard, so restoration of terminal
            // flags always happens first, including early returns and unwinds.
            for (signal, previous) in &self.previous {
                unsafe {
                    libc::sigaction(*signal, previous, std::ptr::null_mut());
                }
            }
            let pending = PENDING_SIGNALS.swap(0, Ordering::Relaxed);
            for (signal, _) in &self.previous {
                if pending & signal_bit(*signal) == 0 {
                    continue;
                }
                // Deliver to the process after restoring its original handlers.
                // This also preserves delivery when a Tokio/background thread
                // received the original signal while the prompt thread waited.
                unsafe {
                    libc::kill(libc::getpid(), *signal);
                }
            }
        }
    }

    struct PrivateTerminal {
        file: File,
        original: libc::termios,
    }
    impl PrivateTerminal {
        fn open() -> Result<Self> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                // Paused terminal output must not block cancellation while
                // printing the prompt or its final newline.
                .custom_flags(libc::O_NONBLOCK)
                .open("/dev/tty")
                .map_err(|_| Error::Crypto("reader requires a private interactive terminal"))?;
            let fd = file.as_raw_fd();
            // Only operate on the explicitly attached terminal. No native
            // authentication store or environment-selected program is involved.
            if unsafe { libc::isatty(fd) } != 1 {
                return Err(Error::Crypto(
                    "reader requires a private interactive terminal",
                ));
            }
            if unsafe { libc::tcgetpgrp(fd) } != unsafe { libc::getpgrp() } {
                return Err(Error::Crypto(
                    "reader requires a foreground private interactive terminal",
                ));
            }
            let mut original = MaybeUninit::<libc::termios>::uninit();
            if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
                return Err(Error::Crypto("cannot configure private reader terminal"));
            }
            let original = unsafe { original.assume_init() };
            let mut private = original;
            private.c_iflag &= !(libc::IXON | libc::IXOFF);
            private.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG);
            private.c_cc[libc::VMIN] = 1;
            private.c_cc[libc::VTIME] = 0;
            // Never drain terminal output: Ctrl-S can suspend it indefinitely.
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &private) } != 0 {
                return Err(Error::Crypto("cannot configure private reader terminal"));
            }
            let terminal = Self { file, original };
            if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
                return Err(Error::Crypto("cannot configure private reader terminal"));
            }
            Ok(terminal)
        }
    }
    impl Drop for PrivateTerminal {
        fn drop(&mut self) {
            // Discard any unfinished private input before restoring shell echo.
            unsafe {
                libc::tcflush(self.file.as_raw_fd(), libc::TCIFLUSH);
                libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.original);
            }
        }
    }

    struct PrivateInput {
        file: File,
        bytes: usize,
    }
    impl Read for PrivateInput {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if output.is_empty() {
                return Ok(0);
            }
            loop {
                if PENDING_SIGNALS.load(Ordering::Relaxed) != 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let mut descriptor = libc::pollfd {
                    fd: self.file.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // Polling makes cancellation bounded even if another thread
                // handled the signal and this thread's read was not interrupted.
                let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
                if ready < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
                if ready <= 0 {
                    continue;
                }
                if PENDING_SIGNALS.load(Ordering::Relaxed) != 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let n = match self.file.read(&mut output[..1]) {
                    Ok(n) => n,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if n == 0 {
                    if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                        return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                    }
                    // POSIX permits a nonblocking terminal to return zero when
                    // no input is available. Mac readiness can be spurious.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                // Intercept cancellation before rpassword raises SIGINT: that
                // would terminate the process before our terminal guard drops.
                if matches!(output[0], 3 | 4) {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                self.bytes += 1;
                if self.bytes > 4096 {
                    return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
                }
                return Ok(n);
            }
        }
    }

    pub(super) fn prompt(prompt: &str) -> Result<Zeroizing<String>> {
        let _signals = PromptSignals::install()?;
        let mut terminal = PrivateTerminal::open()?;
        let input = PrivateInput {
            file: terminal.file.try_clone()?,
            bytes: 0,
        };
        let config = rpassword::ConfigBuilder::new()
            .input_reader(input)
            .output_writer(terminal.file.try_clone()?)
            .password_feedback_hide()
            .build();
        // Echo and signal generation are already disabled when the caller can
        // first see this prompt, including immediately pasted responses.
        terminal.file.write_all(prompt.as_bytes())?;
        terminal.file.flush()?;
        let result = rpassword::read_password_with_config(config)
            .map(Zeroizing::new)
            .map_err(|_| Error::Crypto("private reader input was cancelled or unavailable"));
        let _ = terminal.file.write_all(b"\n");
        result
    }
}
