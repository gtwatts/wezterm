//! Terminal recording and replay in asciinema v2 format.
//!
//! Captures terminal output events with timestamps for later replay.
//! Includes custom marker events for agent annotations (thinking, tool calls).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Event type codes used in asciinema v2 format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    /// Terminal output ("o").
    Output,
    /// Terminal input ("i").
    Input,
    /// Agent marker/annotation ("m").
    Marker,
}

impl EventType {
    /// Return the single-character code used in the .cast file.
    pub fn code(&self) -> &'static str {
        match self {
            EventType::Output => "o",
            EventType::Input => "i",
            EventType::Marker => "m",
        }
    }
}

/// A single timestamped event in a recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingEvent {
    /// Seconds since recording start.
    pub time: f64,
    /// The type of event.
    pub event_type: EventType,
    /// The event payload (output text, input text, or marker JSON).
    pub data: String,
}

/// Environment metadata for the recording header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingEnv {
    /// Shell name.
    #[serde(rename = "SHELL")]
    pub shell: String,
    /// Terminal type.
    #[serde(rename = "TERM")]
    pub term: String,
}

/// The header block of an asciinema v2 recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingHeader {
    /// Format version (always 2).
    pub version: u8,
    /// Terminal width in columns.
    pub width: u32,
    /// Terminal height in rows.
    pub height: u32,
    /// Unix timestamp of recording start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    /// Environment variables.
    pub env: RecordingEnv,
}

/// A complete recording (header + events).
#[derive(Debug, Clone)]
pub struct Recording {
    /// File header.
    pub header: RecordingHeader,
    /// Ordered list of events.
    pub events: Vec<RecordingEvent>,
}

impl Recording {
    /// Total duration of the recording in seconds.
    pub fn duration(&self) -> f64 {
        self.events.last().map(|e| e.time).unwrap_or(0.0)
    }
}

/// Controls terminal recording sessions.
pub struct SessionRecorder {
    /// The active recording, if any.
    recording: Option<Recording>,
    /// When the current recording started.
    start_time: Option<Instant>,
    /// Whether recording is paused.
    paused: bool,
    /// Accumulated pause time to subtract from timestamps.
    pause_offset: Duration,
    /// When the current pause started.
    pause_start: Option<Instant>,
    /// Where the recording will be saved.
    output_path: Option<PathBuf>,
}

impl SessionRecorder {
    /// Create a new recorder (not recording).
    pub fn new() -> Self {
        Self {
            recording: None,
            start_time: None,
            paused: false,
            pause_offset: Duration::ZERO,
            pause_start: None,
            output_path: None,
        }
    }

    /// Start recording with the default path `~/.elwood/recordings/YYYY-MM-DD_HH-MM-SS.cast`.
    ///
    /// Returns the path where the recording will be saved.
    pub fn start(&mut self, width: u32, height: u32) -> PathBuf {
        let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("{timestamp}.cast");
        let dir = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".elwood")
            .join("recordings");
        let path = dir.join(filename);
        self.start_to(path.clone(), width, height);
        path
    }

    /// Start recording to a specific path.
    pub fn start_to(&mut self, path: PathBuf, width: u32, height: u32) {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .ok();

        let header = RecordingHeader {
            version: 2,
            width,
            height,
            timestamp: now_unix,
            env: RecordingEnv {
                shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()),
                term: "xterm-256color".to_string(),
            },
        };

        self.recording = Some(Recording {
            header,
            events: Vec::new(),
        });
        self.start_time = Some(Instant::now());
        self.paused = false;
        self.pause_offset = Duration::ZERO;
        self.pause_start = None;
        self.output_path = Some(path);
    }

    /// Stop recording, save to disk, and return the path.
    pub fn stop(&mut self) -> Option<PathBuf> {
        // If paused, finalize the pause offset
        if let Some(ps) = self.pause_start.take() {
            self.pause_offset += ps.elapsed();
        }
        self.paused = false;

        let path = self.output_path.take()?;
        if let Some(ref recording) = self.recording {
            if let Err(e) = save(recording, &path) {
                tracing::error!("Failed to save recording to {}: {e}", path.display());
            }
        }
        self.recording = None;
        self.start_time = None;
        Some(path)
    }

    /// Pause recording. Events are not recorded while paused.
    pub fn pause(&mut self) {
        if self.recording.is_some() && !self.paused {
            self.paused = true;
            self.pause_start = Some(Instant::now());
        }
    }

    /// Resume recording after a pause.
    pub fn resume(&mut self) {
        if self.paused {
            if let Some(ps) = self.pause_start.take() {
                self.pause_offset += ps.elapsed();
            }
            self.paused = false;
        }
    }

    /// Whether a recording session is active.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Whether recording is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Record a terminal output event.
    pub fn record_output(&mut self, data: &str) {
        self.push_event(EventType::Output, data);
    }

    /// Record a terminal input event.
    pub fn record_input(&mut self, data: &str) {
        self.push_event(EventType::Input, data);
    }

    /// Record an agent marker/annotation event.
    pub fn record_marker(&mut self, marker: &str) {
        self.push_event(EventType::Marker, marker);
    }

    /// Return the number of recorded events (0 if not recording).
    pub fn event_count(&self) -> usize {
        self.recording.as_ref().map(|r| r.events.len()).unwrap_or(0)
    }

    /// Return the duration in seconds since recording started (excluding pauses).
    pub fn elapsed_secs(&self) -> f64 {
        self.start_time
            .map(|st| {
                let raw = st.elapsed();
                let adjusted = raw.saturating_sub(self.pause_offset);
                adjusted.as_secs_f64()
            })
            .unwrap_or(0.0)
    }

    /// Push an event into the recording.
    fn push_event(&mut self, event_type: EventType, data: &str) {
        if self.paused {
            return;
        }
        if let (Some(ref mut recording), Some(start)) =
            (&mut self.recording, self.start_time)
        {
            let raw_elapsed = start.elapsed();
            let adjusted = raw_elapsed.saturating_sub(self.pause_offset);
            recording.events.push(RecordingEvent {
                time: adjusted.as_secs_f64(),
                event_type,
                data: data.to_string(),
            });
        }
    }
}

impl Default for SessionRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Save a recording to disk in asciinema v2 NDJSON format.
pub fn save(recording: &Recording, path: &Path) -> anyhow::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut out = String::new();

    // Header line
    out.push_str(&serde_json::to_string(&recording.header)?);
    out.push('\n');

    // Event lines: [time, type, data]
    for event in &recording.events {
        let line = serde_json::to_string(&(
            event.time,
            event.event_type.code(),
            &event.data,
        ))?;
        out.push_str(&line);
        out.push('\n');
    }

    std::fs::write(path, out)?;
    Ok(())
}

/// Load a recording from an asciinema v2 .cast file.
pub fn load_recording(path: &Path) -> anyhow::Result<Recording> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines();

    // First line: header
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("Empty recording file"))?;
    let header: RecordingHeader = serde_json::from_str(header_line)?;

    // Remaining lines: events
    let mut events = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let tuple: (f64, String, String) = serde_json::from_str(line)?;
        let event_type = match tuple.1.as_str() {
            "o" => EventType::Output,
            "i" => EventType::Input,
            "m" => EventType::Marker,
            other => anyhow::bail!("Unknown event type: {other}"),
        };
        events.push(RecordingEvent {
            time: tuple.0,
            event_type,
            data: tuple.2,
        });
    }

    Ok(Recording { header, events })
}

/// Format a human-readable summary of a recording.
pub fn format_recording_info(recording: &Recording) -> String {
    let duration = recording.duration();
    let event_count = recording.events.len();
    let w = recording.header.width;
    let h = recording.header.height;
    format!(
        "Recording: {w}x{h}, {event_count} events, {duration:.1}s duration"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recorder_start_stop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("test.cast");

        let mut recorder = SessionRecorder::new();
        assert!(!recorder.is_recording());

        recorder.start_to(path.clone(), 120, 40);
        assert!(recorder.is_recording());

        let saved_path = recorder.stop();
        assert!(!recorder.is_recording());
        assert_eq!(saved_path, Some(path.clone()));
        assert!(path.exists());
    }

    #[test]
    fn test_recorder_record_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("events.cast");

        let mut recorder = SessionRecorder::new();
        recorder.start_to(path.clone(), 80, 24);

        recorder.record_output("$ ls\r\n");
        recorder.record_output("file1.txt  file2.txt\r\n");
        recorder.record_input("ls");
        recorder.record_marker(r#"{"type":"tool_start","name":"ReadFile"}"#);

        assert_eq!(recorder.event_count(), 4);

        recorder.stop();

        // Verify file was written and can be loaded
        let recording = load_recording(&path).unwrap();
        assert_eq!(recording.events.len(), 4);
        assert_eq!(recording.events[0].event_type, EventType::Output);
        assert_eq!(recording.events[0].data, "$ ls\r\n");
        assert_eq!(recording.events[2].event_type, EventType::Input);
        assert_eq!(recording.events[3].event_type, EventType::Marker);

        // Timestamps should be monotonically increasing
        for i in 1..recording.events.len() {
            assert!(recording.events[i].time >= recording.events[i - 1].time);
        }
    }

    #[test]
    fn test_recorder_pause_resume() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("pause.cast");

        let mut recorder = SessionRecorder::new();
        recorder.start_to(path.clone(), 80, 24);

        recorder.record_output("before pause\r\n");
        let _before_time = recorder.elapsed_secs();

        recorder.pause();
        assert!(recorder.is_paused());

        // Events during pause should be dropped
        recorder.record_output("during pause\r\n");

        // Sleep a bit to create a gap
        std::thread::sleep(Duration::from_millis(50));

        recorder.resume();
        assert!(!recorder.is_paused());

        recorder.record_output("after resume\r\n");

        recorder.stop();

        let recording = load_recording(&path).unwrap();
        // Only 2 events: before pause and after resume (not the one during pause)
        assert_eq!(recording.events.len(), 2);
        assert_eq!(recording.events[0].data, "before pause\r\n");
        assert_eq!(recording.events[1].data, "after resume\r\n");

        // The time gap between events should be less than the actual wall time
        // because pause time is subtracted
        let time_gap = recording.events[1].time - recording.events[0].time;
        // The gap should be small since the only real work is the code between record calls
        // (the 50ms sleep was during pause and should be excluded)
        // We just verify it's less than 1 second (generous bound)
        assert!(time_gap < 1.0, "time_gap was {time_gap}, expected < 1.0");
    }

    #[test]
    fn test_recorder_asciinema_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("format.cast");

        let mut recorder = SessionRecorder::new();
        recorder.start_to(path.clone(), 120, 40);
        recorder.record_output("hello\r\n");
        recorder.stop();

        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines = content.lines();

        // First line: JSON header
        let header_line = lines.next().unwrap();
        let header: serde_json::Value = serde_json::from_str(header_line).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 120);
        assert_eq!(header["height"], 40);
        assert!(header["timestamp"].is_number());
        assert!(header["env"]["SHELL"].is_string());

        // Second line: event tuple [time, "o", "hello\r\n"]
        let event_line = lines.next().unwrap();
        let event: serde_json::Value = serde_json::from_str(event_line).unwrap();
        assert!(event[0].is_f64());
        assert_eq!(event[1], "o");
        assert_eq!(event[2], "hello\r\n");
    }

    #[test]
    fn test_load_recording() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("roundtrip.cast");

        let mut recorder = SessionRecorder::new();
        recorder.start_to(path.clone(), 100, 30);
        recorder.record_output("output1\r\n");
        recorder.record_input("input1");
        recorder.record_marker("marker1");
        recorder.stop();

        let recording = load_recording(&path).unwrap();
        assert_eq!(recording.header.version, 2);
        assert_eq!(recording.header.width, 100);
        assert_eq!(recording.header.height, 30);
        assert_eq!(recording.events.len(), 3);
        assert_eq!(recording.events[0].data, "output1\r\n");
        assert_eq!(recording.events[1].data, "input1");
        assert_eq!(recording.events[2].data, "marker1");
    }

    #[test]
    fn test_recorder_default_path() {
        let mut recorder = SessionRecorder::new();
        let path = recorder.start(80, 24);

        // Path should match ~/.elwood/recordings/YYYY-MM-DD_HH-MM-SS.cast
        let path_str = path.to_string_lossy();
        assert!(path_str.contains(".elwood"));
        assert!(path_str.contains("recordings"));
        assert!(path_str.ends_with(".cast"));

        // Verify the date pattern
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(
            filename.len() >= 23, // "YYYY-MM-DD_HH-MM-SS.cast" = 24 chars
            "filename was: {filename}"
        );

        // Clean up without saving (just drop)
        recorder.recording = None;
        recorder.output_path = None;
    }

    #[test]
    fn test_marker_event_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("marker.cast");

        let mut recorder = SessionRecorder::new();
        recorder.start_to(path.clone(), 80, 24);
        recorder.record_marker(r#"{"type":"tool_start","name":"BashTool"}"#);
        recorder.stop();

        let content = std::fs::read_to_string(&path).unwrap();
        let event_line = content.lines().nth(1).unwrap();
        let event: serde_json::Value = serde_json::from_str(event_line).unwrap();
        assert_eq!(event[1], "m");
    }

    #[test]
    fn test_recording_info() {
        let recording = Recording {
            header: RecordingHeader {
                version: 2,
                width: 120,
                height: 40,
                timestamp: Some(1700000000),
                env: RecordingEnv {
                    shell: "/bin/zsh".to_string(),
                    term: "xterm-256color".to_string(),
                },
            },
            events: vec![
                RecordingEvent {
                    time: 0.0,
                    event_type: EventType::Output,
                    data: "hello".to_string(),
                },
                RecordingEvent {
                    time: 1.5,
                    event_type: EventType::Output,
                    data: "world".to_string(),
                },
                RecordingEvent {
                    time: 3.2,
                    event_type: EventType::Input,
                    data: "cmd".to_string(),
                },
            ],
        };

        let info = format_recording_info(&recording);
        assert!(info.contains("120x40"));
        assert!(info.contains("3 events"));
        assert!(info.contains("3.2s"));
    }

    #[test]
    fn test_recorder_not_recording() {
        let mut recorder = SessionRecorder::new();
        // These should be no-ops when not recording
        recorder.record_output("test");
        recorder.record_input("test");
        recorder.record_marker("test");
        assert_eq!(recorder.event_count(), 0);
        assert!(!recorder.is_recording());

        // Stop should return None
        assert!(recorder.stop().is_none());
    }
}
