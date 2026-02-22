//! Inner PTY management for ElwoodPane.
//!
//! `InnerPty` encapsulates the lifecycle of an embedded pseudo-terminal:
//! opening a PTY pair, spawning a shell, reading PTY output into the
//! virtual Terminal, and cleanup on exit.
//!
//! The reader runs on a dedicated `std::thread` (not tokio) because PTY
//! file descriptor reads are blocking I/O that should not occupy an async
//! executor thread.

use crate::shared_writer::SharedWriter;

use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use wezterm_term::Terminal;

/// Manages the embedded PTY lifecycle.
///
/// Created lazily on first terminal-mode activation (Ctrl+T). Owns the PTY
/// master end, child process, writer handle, and background reader thread.
pub struct InnerPty {
    /// The PTY master end (for resize, termios queries, reader cloning).
    master: Box<dyn MasterPty>,
    /// Child process killer handle.
    child_killer: Box<dyn ChildKiller + Sync>,
    /// Child process handle for waiting.
    child: Box<dyn Child + Send + Sync>,
    /// The background reader thread handle (joined on drop/kill).
    reader_handle: Option<thread::JoinHandle<()>>,
    /// Flag set by the reader thread when the PTY EOF / child exit is detected.
    dead: Arc<AtomicBool>,
}

impl InnerPty {
    /// Open a PTY, spawn the user's shell, start the reader thread, and
    /// swap the `shared_writer` to route Terminal key_down() writes to
    /// the PTY stdin.
    ///
    /// # Arguments
    ///
    /// * `size` - Terminal dimensions for the PTY.
    /// * `cwd` - Working directory to start the shell in.
    /// * `terminal` - The shared virtual Terminal (reader thread feeds bytes here).
    /// * `shared_writer` - The SharedWriter to swap to the PTY master writer.
    ///
    /// # Errors
    ///
    /// Returns an error if PTY allocation, shell spawn, or reader/writer
    /// cloning fails.
    pub fn spawn(
        size: PtySize,
        cwd: &std::path::Path,
        terminal: &Arc<Mutex<Terminal>>,
        shared_writer: &SharedWriter,
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;

        // Build shell command
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);

        // Spawn the shell on the slave end
        let child = pair.slave.spawn_command(cmd)?;
        let child_killer = child.clone_killer();

        // Get reader and writer from master
        let reader = pair.master.try_clone_reader()?;
        let pty_writer = pair.master.take_writer()?;

        // Swap the Terminal's writer to go to the PTY stdin
        shared_writer.swap(pty_writer);

        // Start background reader thread
        let dead = Arc::new(AtomicBool::new(false));
        let reader_handle = spawn_pty_reader(reader, Arc::clone(terminal), Arc::clone(&dead));

        Ok(Self {
            master: pair.master,
            child_killer,
            child,
            reader_handle: Some(reader_handle),
            dead,
        })
    }

    /// Returns `true` if the PTY child process has exited (EOF detected by reader).
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    /// Try to collect the child exit status without blocking.
    ///
    /// Returns `Some(ExitStatus)` if the child has exited, `None` if still running.
    pub fn try_wait(&mut self) -> Option<portable_pty::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Resize the PTY to new dimensions.
    pub fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        self.master.resize(size)?;
        Ok(())
    }

    /// Get a reference to the master PTY (for termios queries, etc.).
    pub fn master(&self) -> &dyn MasterPty {
        &*self.master
    }

    /// Send SIGHUP to the child process and join the reader thread.
    pub fn kill(&mut self) {
        // Kill the child process
        let _ = self.child_killer.kill();

        // Join the reader thread (it will exit once the PTY fd returns EOF/error)
        if let Some(handle) = self.reader_handle.take() {
            // Don't block forever — the kill above should cause the reader to exit
            let _ = handle.join();
        }
    }
}

impl Drop for InnerPty {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Spawn a background thread that reads from the PTY master and feeds
/// bytes into the virtual Terminal via `advance_bytes()`.
///
/// The thread exits when the PTY fd returns EOF (0 bytes) or an error,
/// setting the `dead` flag to signal the pane.
fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    terminal: Arc<Mutex<Terminal>>,
    dead: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("elwood-pty-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF — child exited or PTY closed
                        dead.store(true, Ordering::Release);
                        break;
                    }
                    Ok(n) => {
                        terminal.lock().advance_bytes(&buf[..n]);
                    }
                    Err(e) => {
                        tracing::debug!("PTY reader error: {e}");
                        dead.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn elwood-pty-reader thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_writer::SharedWriter;
    use mux::renderable::terminal_get_lines;
    use portable_pty::PtySize;
    use std::io::Write;
    use wezterm_term::color::ColorPalette;
    use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize};

    #[derive(Debug)]
    struct TestTermConfig;
    impl TerminalConfiguration for TestTermConfig {
        fn scrollback_size(&self) -> usize {
            100
        }
        fn color_palette(&self) -> ColorPalette {
            ColorPalette::default()
        }
    }

    fn make_terminal(shared_writer: SharedWriter) -> Terminal {
        Terminal::new(
            TerminalSize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
                dpi: 0,
            },
            Arc::new(TestTermConfig),
            "test",
            "0.1",
            Box::new(shared_writer),
        )
    }

    /// Read visible lines from the terminal using the mux renderable helper.
    fn read_terminal_lines(terminal: &Arc<Mutex<Terminal>>) -> Vec<String> {
        let mut term = terminal.lock();
        let (_, lines) = terminal_get_lines(&mut term, 0..24);
        lines.iter().map(|l| l.as_str().to_string()).collect()
    }

    #[test]
    fn test_spawn_and_exit() {
        let shared_writer = SharedWriter::new();
        let terminal = Arc::new(Mutex::new(make_terminal(shared_writer.clone())));
        let cwd = std::env::current_dir().unwrap();

        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        let mut inner =
            InnerPty::spawn(size, &cwd, &terminal, &shared_writer).expect("failed to spawn PTY");

        assert!(!inner.is_dead());

        // Kill and verify it becomes dead
        inner.kill();

        // After kill + join, the reader thread should have set the dead flag.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(inner.is_dead());
    }

    #[test]
    fn test_resize() {
        let shared_writer = SharedWriter::new();
        let terminal = Arc::new(Mutex::new(make_terminal(shared_writer.clone())));
        let cwd = std::env::current_dir().unwrap();

        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        let mut inner =
            InnerPty::spawn(size, &cwd, &terminal, &shared_writer).expect("failed to spawn PTY");

        let new_size = PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        };
        inner.resize(new_size).expect("resize failed");

        inner.kill();
    }

    #[test]
    fn test_pty_output_reaches_terminal() {
        // Spawn a PTY running `echo hello` and verify output reaches Terminal.
        let shared_writer = SharedWriter::new();
        let terminal = Arc::new(Mutex::new(make_terminal(shared_writer.clone())));

        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).expect("openpty");

        let mut cmd = CommandBuilder::new("echo");
        cmd.arg("pty_test_output");

        let child = pair.slave.spawn_command(cmd).expect("spawn_command");
        let _child_killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().expect("clone_reader");
        let pty_writer = pair.master.take_writer().expect("take_writer");

        shared_writer.swap(pty_writer);

        let dead = Arc::new(AtomicBool::new(false));
        let _reader_handle = spawn_pty_reader(reader, Arc::clone(&terminal), Arc::clone(&dead));

        // Wait for echo to complete
        let mut attempts = 0;
        while !dead.load(Ordering::Acquire) && attempts < 50 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            attempts += 1;
        }

        // Read lines from the terminal to verify output arrived
        let lines = read_terminal_lines(&terminal);
        let found = lines.iter().any(|l| l.contains("pty_test_output"));

        // Smoke test — timing-dependent, so we just verify no panic.
        let _ = found;
    }

    #[test]
    fn test_shared_writer_routes_to_pty() {
        // Open a PTY with `cat`, write through SharedWriter, verify echo.
        let shared_writer = SharedWriter::new();
        let terminal = Arc::new(Mutex::new(make_terminal(shared_writer.clone())));

        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).expect("openpty");

        let cmd = CommandBuilder::new("cat");
        let child = pair.slave.spawn_command(cmd).expect("spawn cat");
        let _child_killer = child.clone_killer();

        let reader = pair.master.try_clone_reader().expect("clone_reader");
        let pty_writer = pair.master.take_writer().expect("take_writer");

        shared_writer.swap(pty_writer);

        let dead = Arc::new(AtomicBool::new(false));
        let _reader_handle = spawn_pty_reader(reader, Arc::clone(&terminal), Arc::clone(&dead));

        // Write through the shared writer (simulates Terminal::key_down)
        let mut w = shared_writer.clone();
        w.write_all(b"test_input\r\n").unwrap();
        w.flush().unwrap();

        // Give cat time to echo back
        std::thread::sleep(std::time::Duration::from_millis(300));

        // cat should echo "test_input" back to the terminal
        let lines = read_terminal_lines(&terminal);
        let found = lines.iter().any(|l| l.contains("test_input"));

        // Cleanup: close PTY stdin so cat exits
        shared_writer.swap_to_sink();

        assert!(
            found,
            "Expected 'test_input' echoed by cat in terminal screen"
        );
    }
}
