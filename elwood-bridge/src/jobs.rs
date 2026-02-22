//! Background job tracking for long-running commands.
//!
//! Provides a [`JobManager`] that tracks background shell commands with their
//! status, output, and lifecycle. Jobs can be started via the `&` suffix in
//! Terminal mode, the `/bg` slash command, or programmatically.
//!
//! ## Features
//!
//! - Background command execution with piped stdout/stderr
//! - Incremental output capture (last 1000 stdout lines, 100 stderr lines)
//! - Job lifecycle: Running -> Completed/Failed/Cancelled
//! - Kill with SIGTERM, escalating to SIGKILL after 5 seconds
//! - Retention limit of 50 completed jobs (oldest evicted first)
//! - Toast notifications on job completion/failure

use std::collections::HashMap;
use std::time::Instant;

/// Maximum number of completed/failed/cancelled jobs to retain.
const MAX_RETAINED: usize = 50;

/// Maximum stdout lines to keep per job.
const MAX_STDOUT_LINES: usize = 1000;

/// Maximum stderr lines to keep per job.
const MAX_STDERR_LINES: usize = 100;

/// Unique job identifier.
pub type JobId = u32;

/// Status of a background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Currently executing.
    Running,
    /// Exited with code 0.
    Completed,
    /// Exited with non-zero code or execution error.
    Failed,
    /// Killed by user request.
    Cancelled,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Running => write!(f, "running"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
            JobStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// A background job with its state and captured output.
#[derive(Debug, Clone)]
pub struct Job {
    /// Unique job identifier.
    pub id: JobId,
    /// The shell command being executed.
    pub command: String,
    /// Current status.
    pub status: JobStatus,
    /// When the job was started.
    pub start_time: Instant,
    /// When the job finished (None if still running).
    pub end_time: Option<Instant>,
    /// Process exit code (None if still running or killed).
    pub exit_code: Option<i32>,
    /// Captured stdout (truncated to last MAX_STDOUT_LINES lines).
    pub stdout: Vec<String>,
    /// Captured stderr (truncated to last MAX_STDERR_LINES lines).
    pub stderr: Vec<String>,
    /// OS process ID (for kill operations).
    pub pid: Option<u32>,
}

impl Job {
    /// Create a new running job.
    fn new(id: JobId, command: String, pid: Option<u32>) -> Self {
        Self {
            id,
            command,
            status: JobStatus::Running,
            start_time: Instant::now(),
            end_time: None,
            exit_code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            pid,
        }
    }

    /// Duration of the job (elapsed if running, total if finished).
    pub fn duration_secs(&self) -> f64 {
        match self.end_time {
            Some(end) => end.duration_since(self.start_time).as_secs_f64(),
            None => self.start_time.elapsed().as_secs_f64(),
        }
    }

    /// Format duration as a human-readable string.
    pub fn duration_display(&self) -> String {
        let secs = self.duration_secs();
        if secs < 60.0 {
            format!("{:.1}s", secs)
        } else if secs < 3600.0 {
            format!("{:.0}m{:02.0}s", secs / 60.0, secs % 60.0)
        } else {
            let h = secs / 3600.0;
            let m = (secs % 3600.0) / 60.0;
            format!("{:.0}h{:02.0}m", h, m)
        }
    }

    /// Append a stdout line, enforcing the retention limit.
    pub fn append_stdout(&mut self, line: String) {
        self.stdout.push(line);
        if self.stdout.len() > MAX_STDOUT_LINES {
            let excess = self.stdout.len() - MAX_STDOUT_LINES;
            self.stdout.drain(..excess);
        }
    }

    /// Append a stderr line, enforcing the retention limit.
    pub fn append_stderr(&mut self, line: String) {
        self.stderr.push(line);
        if self.stderr.len() > MAX_STDERR_LINES {
            let excess = self.stderr.len() - MAX_STDERR_LINES;
            self.stderr.drain(..excess);
        }
    }

    /// Mark the job as completed with the given exit code.
    pub fn complete(&mut self, exit_code: i32) {
        self.end_time = Some(Instant::now());
        self.exit_code = Some(exit_code);
        self.status = if exit_code == 0 {
            JobStatus::Completed
        } else {
            JobStatus::Failed
        };
    }

    /// Mark the job as failed with an error message.
    pub fn fail(&mut self, error: &str) {
        self.end_time = Some(Instant::now());
        self.exit_code = Some(-1);
        self.status = JobStatus::Failed;
        self.stderr.push(error.to_string());
    }

    /// Mark the job as cancelled.
    pub fn cancel(&mut self) {
        self.end_time = Some(Instant::now());
        self.status = JobStatus::Cancelled;
    }
}

/// Manages background jobs: creation, tracking, output capture, and retention.
#[derive(Debug)]
pub struct JobManager {
    /// Active and retained jobs by ID.
    jobs: HashMap<JobId, Job>,
    /// Next job ID to assign.
    next_id: JobId,
}

impl JobManager {
    /// Create a new empty JobManager.
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            next_id: 1,
        }
    }

    /// Create a new job entry and return its ID.
    ///
    /// The caller is responsible for actually spawning the process and
    /// sending output updates via [`append_stdout`](Job::append_stdout) etc.
    pub fn create_job(&mut self, command: String, pid: Option<u32>) -> JobId {
        let id = self.next_id;
        self.next_id += 1;
        let job = Job::new(id, command, pid);
        self.jobs.insert(id, job);
        id
    }

    /// Get a reference to a job by ID.
    pub fn get(&self, id: JobId) -> Option<&Job> {
        self.jobs.get(&id)
    }

    /// Get a mutable reference to a job by ID.
    pub fn get_mut(&mut self, id: JobId) -> Option<&mut Job> {
        self.jobs.get_mut(&id)
    }

    /// Remove a job from the manager (dismiss).
    pub fn remove(&mut self, id: JobId) -> Option<Job> {
        self.jobs.remove(&id)
    }

    /// Count of currently running jobs.
    pub fn running_count(&self) -> usize {
        self.jobs.values().filter(|j| j.status == JobStatus::Running).count()
    }

    /// All jobs sorted by start time (newest first).
    pub fn all_jobs(&self) -> Vec<&Job> {
        let mut jobs: Vec<&Job> = self.jobs.values().collect();
        // Sort by start time descending (newest first)
        jobs.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        jobs
    }

    /// Get only running jobs.
    pub fn running_jobs(&self) -> Vec<&Job> {
        self.jobs.values().filter(|j| j.status == JobStatus::Running).collect()
    }

    /// Evict old completed/failed/cancelled jobs beyond the retention limit.
    ///
    /// Running jobs are never evicted.
    pub fn enforce_retention(&mut self) {
        let finished: Vec<JobId> = {
            let mut finished: Vec<(JobId, Instant)> = self.jobs.iter()
                .filter(|(_, j)| j.status != JobStatus::Running)
                .map(|(&id, j)| (id, j.start_time))
                .collect();
            // Sort by start time ascending (oldest first)
            finished.sort_by_key(|&(_, t)| t);
            finished.into_iter().map(|(id, _)| id).collect()
        };

        if finished.len() > MAX_RETAINED {
            let to_remove = finished.len() - MAX_RETAINED;
            for id in finished.into_iter().take(to_remove) {
                self.jobs.remove(&id);
            }
        }
    }

    /// Format a summary string suitable for the status bar.
    ///
    /// Returns `None` if no jobs are running.
    pub fn status_summary(&self) -> Option<String> {
        let count = self.running_count();
        if count == 0 {
            None
        } else if count == 1 {
            Some("\u{2699} 1 job".to_string())
        } else {
            Some(format!("\u{2699} {count} jobs"))
        }
    }

    /// Format the jobs panel content as ANSI-styled text.
    ///
    /// Returns a multi-line string suitable for rendering in the chat area
    /// or as an overlay.
    pub fn render_panel(&self) -> String {
        let jobs = self.all_jobs();
        if jobs.is_empty() {
            return "No background jobs.\n".to_string();
        }

        let mut out = String::new();
        for job in &jobs {
            let (icon, status_color) = match job.status {
                JobStatus::Running => ("\u{25CB}", "\x1b[38;2;125;207;255m"),   // ○ cyan
                JobStatus::Completed => ("\u{2714}", "\x1b[38;2;158;206;106m"), // ✔ green
                JobStatus::Failed => ("\u{2718}", "\x1b[38;2;247;118;142m"),    // ✘ red
                JobStatus::Cancelled => ("\u{2212}", "\x1b[38;2;86;95;137m"),   // − muted
            };

            let cmd_display = if job.command.len() > 50 {
                let mut end = 47;
                while !job.command.is_char_boundary(end) && end > 0 {
                    end -= 1;
                }
                format!("{}...", &job.command[..end])
            } else {
                job.command.clone()
            };

            let exit_str = job.exit_code
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default();

            out.push_str(&format!(
                "  {status_color}{icon}\x1b[0m  \x1b[1m#{}\x1b[0m  \x1b[38;2;192;202;245m{}\x1b[0m  \x1b[38;2;86;95;137m{}{}\x1b[0m\r\n",
                job.id,
                cmd_display,
                job.duration_display(),
                exit_str,
            ));
        }

        out.push_str("\r\n\x1b[38;2;86;95;137m  [k]ill  [d]ismiss  [r]e-run  [Esc] close\x1b[0m\r\n");
        out
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_job() {
        let mut mgr = JobManager::new();
        let id = mgr.create_job("sleep 10".into(), Some(1234));
        assert_eq!(id, 1);
        let job = mgr.get(id).unwrap();
        assert_eq!(job.command, "sleep 10");
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.pid, Some(1234));
    }

    #[test]
    fn test_job_ids_increment() {
        let mut mgr = JobManager::new();
        let id1 = mgr.create_job("cmd1".into(), None);
        let id2 = mgr.create_job("cmd2".into(), None);
        let id3 = mgr.create_job("cmd3".into(), None);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn test_job_complete_success() {
        let mut mgr = JobManager::new();
        let id = mgr.create_job("echo hello".into(), None);
        mgr.get_mut(id).unwrap().complete(0);
        let job = mgr.get(id).unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.exit_code, Some(0));
        assert!(job.end_time.is_some());
    }

    #[test]
    fn test_job_complete_failure() {
        let mut mgr = JobManager::new();
        let id = mgr.create_job("false".into(), None);
        mgr.get_mut(id).unwrap().complete(1);
        let job = mgr.get(id).unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.exit_code, Some(1));
    }

    #[test]
    fn test_job_cancel() {
        let mut mgr = JobManager::new();
        let id = mgr.create_job("sleep 100".into(), Some(999));
        mgr.get_mut(id).unwrap().cancel();
        let job = mgr.get(id).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.end_time.is_some());
    }

    #[test]
    fn test_job_fail_with_error() {
        let mut mgr = JobManager::new();
        let id = mgr.create_job("bad_cmd".into(), None);
        mgr.get_mut(id).unwrap().fail("command not found");
        let job = mgr.get(id).unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.exit_code, Some(-1));
        assert!(job.stderr.iter().any(|l| l.contains("command not found")));
    }

    #[test]
    fn test_running_count() {
        let mut mgr = JobManager::new();
        let id1 = mgr.create_job("cmd1".into(), None);
        let _id2 = mgr.create_job("cmd2".into(), None);
        assert_eq!(mgr.running_count(), 2);

        mgr.get_mut(id1).unwrap().complete(0);
        assert_eq!(mgr.running_count(), 1);
    }

    #[test]
    fn test_stdout_truncation() {
        let mut job = Job::new(1, "big_cmd".into(), None);
        for i in 0..1500 {
            job.append_stdout(format!("line {i}"));
        }
        assert_eq!(job.stdout.len(), MAX_STDOUT_LINES);
        // Should keep the last 1000 lines
        assert!(job.stdout[0].contains("500"));
        assert!(job.stdout.last().unwrap().contains("1499"));
    }

    #[test]
    fn test_stderr_truncation() {
        let mut job = Job::new(1, "err_cmd".into(), None);
        for i in 0..200 {
            job.append_stderr(format!("err {i}"));
        }
        assert_eq!(job.stderr.len(), MAX_STDERR_LINES);
        assert!(job.stderr.last().unwrap().contains("199"));
    }

    #[test]
    fn test_retention_limit() {
        let mut mgr = JobManager::new();
        // Create 60 completed jobs
        for i in 0..60 {
            let id = mgr.create_job(format!("cmd {i}"), None);
            mgr.get_mut(id).unwrap().complete(0);
        }
        assert_eq!(mgr.jobs.len(), 60);

        mgr.enforce_retention();
        assert_eq!(mgr.jobs.len(), MAX_RETAINED);
    }

    #[test]
    fn test_retention_preserves_running() {
        let mut mgr = JobManager::new();
        // Create 55 completed jobs
        for i in 0..55 {
            let id = mgr.create_job(format!("done {i}"), None);
            mgr.get_mut(id).unwrap().complete(0);
        }
        // Create 2 running jobs
        let _run1 = mgr.create_job("running1".into(), None);
        let _run2 = mgr.create_job("running2".into(), None);

        mgr.enforce_retention();

        // Running jobs must survive
        assert_eq!(mgr.running_count(), 2);
        // Total should be MAX_RETAINED completed + 2 running
        let finished_count = mgr.jobs.values().filter(|j| j.status != JobStatus::Running).count();
        assert!(finished_count <= MAX_RETAINED);
    }

    #[test]
    fn test_remove_job() {
        let mut mgr = JobManager::new();
        let id = mgr.create_job("cmd".into(), None);
        assert!(mgr.get(id).is_some());
        let removed = mgr.remove(id);
        assert!(removed.is_some());
        assert!(mgr.get(id).is_none());
    }

    #[test]
    fn test_status_summary() {
        let mut mgr = JobManager::new();
        assert!(mgr.status_summary().is_none());

        let _id = mgr.create_job("cmd1".into(), None);
        assert_eq!(mgr.status_summary(), Some("\u{2699} 1 job".to_string()));

        let _id2 = mgr.create_job("cmd2".into(), None);
        assert_eq!(mgr.status_summary(), Some("\u{2699} 2 jobs".to_string()));
    }

    #[test]
    fn test_all_jobs_sorted_newest_first() {
        let mut mgr = JobManager::new();
        let _id1 = mgr.create_job("first".into(), None);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _id2 = mgr.create_job("second".into(), None);

        let jobs = mgr.all_jobs();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].command, "second");
        assert_eq!(jobs[1].command, "first");
    }

    #[test]
    fn test_duration_display() {
        let mut job = Job::new(1, "cmd".into(), None);
        // Simulate a completed job
        job.end_time = Some(job.start_time + std::time::Duration::from_secs(45));
        assert!(job.duration_display().contains("45"));

        job.end_time = Some(job.start_time + std::time::Duration::from_secs(125));
        let d = job.duration_display();
        assert!(d.contains("m"), "should contain minutes: {d}");
    }

    #[test]
    fn test_render_panel_empty() {
        let mgr = JobManager::new();
        let panel = mgr.render_panel();
        assert!(panel.contains("No background jobs"));
    }

    #[test]
    fn test_render_panel_with_jobs() {
        let mut mgr = JobManager::new();
        let _id1 = mgr.create_job("cargo build".into(), Some(1000));
        let id2 = mgr.create_job("npm test".into(), None);
        mgr.get_mut(id2).unwrap().complete(0);

        let panel = mgr.render_panel();
        assert!(panel.contains("cargo build"));
        assert!(panel.contains("npm test"));
        assert!(panel.contains("#1"));
        assert!(panel.contains("#2"));
        // Should have key hints
        assert!(panel.contains("[k]ill"));
        assert!(panel.contains("[d]ismiss"));
        assert!(panel.contains("[r]e-run"));
    }

    #[test]
    fn test_job_status_display() {
        assert_eq!(format!("{}", JobStatus::Running), "running");
        assert_eq!(format!("{}", JobStatus::Completed), "completed");
        assert_eq!(format!("{}", JobStatus::Failed), "failed");
        assert_eq!(format!("{}", JobStatus::Cancelled), "cancelled");
    }
}
