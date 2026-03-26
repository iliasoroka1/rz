//! PTY-based agent wrapper — run agents in any terminal without a multiplexer.
//!
//! Creates a pseudo-terminal, forks the child command, and injects incoming
//! @@RZ: messages into the PTY. Subscribes to NATS for message delivery.
//!
//! Usage: `rz agent --name worker -- claude --dangerously-skip-permissions`

use eyre::{Context, Result};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg, Termios};
use nix::unistd::{close, dup2, execvp, fork, setsid, write, ForkResult, Pid};
use rz_agent_protocol::Envelope;
use std::ffi::CString;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Global flag set by SIGWINCH handler to trigger window-size propagation.
static WINCH_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Global master fd for SIGWINCH handler (set before installing handler).
static MASTER_RAW_FD: AtomicI32 = AtomicI32::new(-1);

/// RAII guard that restores terminal settings on drop.
struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let fd = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = tcsetattr(&fd, SetArg::TCSANOW, &self.original);
    }
}

/// Read the current window size from `from_fd` and set it on `to_fd`.
fn propagate_winsize(from_fd: RawFd, to_fd: RawFd) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(from_fd, libc::TIOCGWINSZ, &mut ws) == 0 {
            let _ = libc::ioctl(to_fd, libc::TIOCSWINSZ, &ws);
        }
    }
}

/// SIGWINCH handler — just sets the flag, actual work is in the main loop.
extern "C" fn sigwinch_handler(_: libc::c_int) {
    WINCH_RECEIVED.store(true, Ordering::SeqCst);
}

/// Run a command as a named rz agent with PTY wrapping.
pub fn run_agent(name: &str, command: &[String], _no_bootstrap: bool) -> Result<()> {
    // Phase 1: Create PTY
    let pty = openpty(None, None).wrap_err("failed to open PTY")?;
    let master_raw: RawFd = pty.master.as_raw_fd();
    let slave_raw: RawFd = pty.slave.as_raw_fd();

    // Phase 2: Register in registry
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    crate::registry::register(crate::registry::AgentEntry {
        name: name.to_string(),
        id: format!("pty-{}", std::process::id()),
        transport: "nats".to_string(),
        endpoint: name.to_string(),
        capabilities: vec![],
        registered_at: now_ms,
        last_seen: now_ms,
    })
    .wrap_err("failed to register agent")?;

    // Phase 3: Fork child
    let child_pid: Pid;
    match unsafe { fork() }.wrap_err("fork failed")? {
        ForkResult::Child => {
            // Close master fd in child
            drop(pty.master);

            // Create new session
            setsid().ok();

            // Dup slave to stdin/stdout/stderr
            dup2(slave_raw, libc::STDIN_FILENO).ok();
            dup2(slave_raw, libc::STDOUT_FILENO).ok();
            dup2(slave_raw, libc::STDERR_FILENO).ok();

            if slave_raw > 2 {
                close(slave_raw).ok();
            }

            // Set env vars
            std::env::set_var("RZ_AGENT_NAME", name);

            // execvp
            let c_cmd = CString::new(command[0].as_str()).expect("invalid command");
            let c_args: Vec<CString> = command
                .iter()
                .map(|a| CString::new(a.as_str()).unwrap())
                .collect();

            #[allow(unreachable_code)]
            {
                execvp(&c_cmd, &c_args).expect("execvp failed");
                std::process::exit(1);
            }
        }
        ForkResult::Parent { child } => {
            child_pid = child;
            drop(pty.slave);
        }
    }

    // Phase 4: Raw mode on user's terminal
    let stdin_fd = std::io::stdin().as_raw_fd();
    let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
    let original_termios = tcgetattr(&stdin_borrowed).wrap_err("tcgetattr failed")?;
    let mut raw = original_termios.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&stdin_borrowed, SetArg::TCSANOW, &raw).wrap_err("tcsetattr failed")?;

    let _raw_guard = RawModeGuard {
        fd: stdin_fd,
        original: original_termios,
    };

    // Phase 5: NATS subscriber thread
    let (msg_tx, msg_rx) = mpsc::channel::<Envelope>();

    let nats_name = name.to_string();
    if crate::nats_hub::hub_url().is_some() {
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("rz: pty: failed to build tokio runtime: {e}");
                    return;
                }
            };

            rt.block_on(async {
                let url = match crate::nats_hub::hub_url() {
                    Some(u) => u,
                    None => return,
                };

                let client = match async_nats::connect(&url).await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("rz: pty: nats connect failed: {e}");
                        return;
                    }
                };

                let subject = format!("agent.{nats_name}");

                // Try JetStream first
                let js = async_nats::jetstream::new(client.clone());
                let stream_name =
                    format!("RZ_{}", nats_name.replace('.', "_").replace('-', "_"));
                let consumer_name = format!("rz_{nats_name}");

                if let Ok(stream) = js.get_stream(&stream_name).await {
                    use async_nats::jetstream::consumer;
                    use futures::StreamExt;

                    if let Ok(consumer) = stream
                        .get_or_create_consumer(
                            &consumer_name,
                            consumer::pull::Config {
                                durable_name: Some(consumer_name.clone()),
                                ack_policy: consumer::AckPolicy::Explicit,
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        if let Ok(mut messages) = consumer.messages().await {
                            eprintln!("rz: pty: listening (jetstream) for '{nats_name}'");
                            while let Some(msg) = messages.next().await {
                                let msg = match msg {
                                    Ok(m) => m,
                                    Err(_) => continue,
                                };
                                let payload =
                                    std::str::from_utf8(&msg.payload).unwrap_or_default();
                                if let Ok(env) = Envelope::decode(payload) {
                                    if msg_tx.send(env).is_err() {
                                        return;
                                    }
                                }
                                let _ = msg.ack().await;
                            }
                            return; // stream ended
                        }
                    }
                }

                // Core NATS fallback
                let mut subscriber = match client
                    .subscribe(async_nats::Subject::from(subject))
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("rz: pty: nats subscribe failed: {e}");
                        return;
                    }
                };

                eprintln!("rz: pty: listening (core nats) for '{nats_name}'");
                while let Some(msg) = futures::StreamExt::next(&mut subscriber).await {
                    let payload = std::str::from_utf8(&msg.payload).unwrap_or_default();
                    if let Ok(env) = Envelope::decode(payload) {
                        if msg_tx.send(env).is_err() {
                            return;
                        }
                    }
                }
            });
        });
    } else {
        eprintln!("rz: pty: RZ_HUB not set — NATS messaging disabled");
    }

    // Phase 6: Propagate window size + install SIGWINCH handler
    propagate_winsize(stdin_fd, master_raw);
    MASTER_RAW_FD.store(master_raw, Ordering::SeqCst);

    let sa = SigAction::new(
        SigHandler::Handler(sigwinch_handler),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGWINCH, &sa).ok();
    }

    // Phase 7: I/O loop
    let mut buf = [0u8; 4096];
    let stdout_fd = std::io::stdout().as_raw_fd();

    loop {
        // Handle SIGWINCH
        if WINCH_RECEIVED.swap(false, Ordering::SeqCst) {
            propagate_winsize(stdin_fd, master_raw);
        }

        // We need to create BorrowedFds for poll each iteration
        let stdin_bfd = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
        let master_bfd = unsafe { BorrowedFd::borrow_raw(master_raw) };

        let mut poll_fds = [
            PollFd::new(stdin_bfd, PollFlags::POLLIN),
            PollFd::new(master_bfd, PollFlags::POLLIN),
        ];

        match poll(&mut poll_fds, PollTimeout::from(50u16)) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                eprintln!("rz: pty: poll error: {e}");
                break;
            }
        }

        // stdin -> master_fd
        if let Some(flags) = poll_fds[0].revents() {
            if flags.intersects(PollFlags::POLLIN) {
                match nix::unistd::read(stdin_fd, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = write_all_fd(master_raw, &buf[..n]);
                    }
                    Err(nix::errno::Errno::EIO) | Err(nix::errno::Errno::EAGAIN) => {}
                    Err(_) => break,
                }
            }
        }

        // master_fd -> stdout
        if let Some(flags) = poll_fds[1].revents() {
            if flags.intersects(PollFlags::POLLIN) {
                match nix::unistd::read(master_raw, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = write_all_fd(stdout_fd, &buf[..n]);
                    }
                    Err(nix::errno::Errno::EIO) => break,
                    Err(nix::errno::Errno::EAGAIN) => {}
                    Err(_) => break,
                }
            }
            if flags.intersects(PollFlags::POLLHUP) {
                // Drain remaining data
                loop {
                    match nix::unistd::read(master_raw, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let _ = write_all_fd(stdout_fd, &buf[..n]);
                        }
                        Err(_) => break,
                    }
                }
                break;
            }
        }

        // Check for incoming NATS messages
        while let Ok(envelope) = msg_rx.try_recv() {
            if let Ok(wire) = envelope.encode() {
                let line = format!("{wire}\r");
                let _ = write_all_fd(master_raw, line.as_bytes());
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    // Phase 8: Cleanup
    drop(_raw_guard);
    let _ = crate::registry::deregister(name);

    use nix::sys::wait::waitpid;
    let _ = waitpid(child_pid, None);

    Ok(())
}

/// Write all bytes to a raw fd, retrying on partial writes.
fn write_all_fd(fd: RawFd, mut data: &[u8]) -> Result<()> {
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    while !data.is_empty() {
        match write(&borrowed, data) {
            Ok(n) => data = &data[n..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::EAGAIN) => continue,
            Err(e) => return Err(e).wrap_err("write failed"),
        }
    }
    Ok(())
}
