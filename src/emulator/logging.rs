//! Structured logging: a record type, a sender with a built-in `log`-crate fallback, a bounded
//! channel to a background writer thread, and a human-readable, unambiguously column-parseable
//! text format — the library-level primitive for writing device/CPU diagnostic messages to a
//! file a UI can tail, without ever stalling CPU execution.

use std::fmt::Write as _;
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Severity of a logged message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Informational message; normal operation.
    Info,
    /// Warning; unexpected but recoverable condition.
    Warn,
    /// Error; something failed.
    Error,
}

impl From<LogLevel> for log::Level {
    fn from(level: LogLevel) -> log::Level {
        match level {
            LogLevel::Info => log::Level::Info,
            LogLevel::Warn => log::Level::Warn,
            LogLevel::Error => log::Level::Error,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        };
        f.write_str(s)
    }
}

/// Broad source of a logged message. `Transport` is included now even though this pass populates
/// no call site with it — the user-facing format only ever needs to support three categories
/// (CPU, device, transport), and reserving the variant now avoids a breaking format-consumer
/// change later for whatever UI panel parses this column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    /// Originated from the CPU itself.
    Cpu,
    /// Originated from a device.
    Device,
    /// Originated from a transport.
    Transport,
}

impl std::fmt::Display for LogCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LogCategory::Cpu => "CPU",
            LogCategory::Device => "Device",
            LogCategory::Transport => "Transport",
        };
        f.write_str(s)
    }
}

/// One logged message: when it was written, how many CPU cycles had accumulated at that point,
/// its severity, its broad source, and free text.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Wall-clock time the record was created.
    pub timestamp: std::time::SystemTime,
    /// CPU cycle count accumulated at the time the record was created.
    pub cycles: u64,
    /// Severity of the message.
    pub level: LogLevel,
    /// Broad source of the message.
    pub category: LogCategory,
    /// Free-text message.
    pub message: String,
}

/// Formats `rec` as a single space-delimited line (including a trailing `\n`) and appends it to
/// `out`: RFC 3339 timestamp (millisecond precision, UTC), cycle count, level, category, then
/// `message` verbatim except that any `\n`/`\r` it contains is escaped to a literal two-character
/// `\n`/`\r` sequence, preserving the one-record-per-line contract.
fn format_record(rec: &LogRecord, out: &mut String) {
    let ts = jiff::Timestamp::try_from(rec.timestamp).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let _ = write!(out, "{ts:.3} {} {} {} ", rec.cycles, rec.level, rec.category);
    for ch in rec.message.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('\n');
}

/// Where a `LogSender` delivers records.
#[derive(Clone)]
enum Sink {
    /// Hands records to a background writer thread over a bounded channel; drops and counts a
    /// record if the channel is full rather than ever blocking the caller.
    File { tx: crossbeam_channel::Sender<LogRecord>, dropped: Arc<AtomicU64> },
    /// No file sink configured: forwards through the `log` crate instead, at the matching
    /// `log::Level`, so nobody's `RUST_LOG`-based visibility regresses by default.
    LogCrate,
}

/// Producer handle: cloned into `Cpu` and into every device that wants to log. Every `Cpu` and
/// device owns one unconditionally (defaulting to the `log`-crate fallback via `Default`), so
/// call sites never branch on whether a real sink is configured — that's `Sink`'s job.
#[derive(Clone)]
pub struct LogSender {
    sink: Sink,
    cycles: Arc<AtomicU64>,
}

impl Default for LogSender {
    /// A sender with no file sink configured; forwards through the `log` crate.
    fn default() -> Self {
        LogSender { sink: Sink::LogCrate, cycles: Default::default() }
    }
}

impl LogSender {
    /// Enqueues (or, with no file sink configured, forwards to the `log` crate) a record built
    /// from `level`/`category`/`message`, stamped with the current time and the live cycle count
    /// (see `set_cycles`). Never blocks the caller.
    pub fn log(&self, level: LogLevel, category: LogCategory, message: impl Into<String>) {
        let message = message.into();
        let cycles = self.cycles.load(Ordering::Relaxed);
        match &self.sink {
            Sink::File { tx, dropped } => {
                let rec = LogRecord { timestamp: std::time::SystemTime::now(), cycles, level, category, message };
                if tx.try_send(rec).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            Sink::LogCrate => {
                log::log!(level.into(), "{cycles} [{category}] {message}");
            }
        }
    }

    /// Updates the live cycle count subsequent `log()` calls (from any clone of this sender)
    /// will be stamped with. Called once per elapsed CPU cycle by whichever `Cpu` owns the
    /// authoritative counter; devices never call this themselves.
    pub fn set_cycles(&self, cycles: u64) {
        self.cycles.store(cycles, Ordering::Relaxed);
    }

    /// Number of records dropped so far because the writer thread's channel was full. Always 0
    /// for a `log`-crate-backed sender (nothing is ever dropped on that path).
    pub fn dropped_count(&self) -> u64 {
        match &self.sink {
            Sink::File { dropped, .. } => dropped.load(Ordering::Relaxed),
            Sink::LogCrate => 0,
        }
    }
}

/// Builds the `Sink::File`-backed plumbing (bounded channel + drop counter) shared by
/// `spawn_log_writer`, `spawn_log_collector`, and the test-only `test_channel_sender`: a
/// `Sink` ready to hand to a `LogSender`, and the `Receiver` half a background thread (or a
/// test) drains.
fn bounded_file_sink(capacity: usize) -> (Sink, crossbeam_channel::Receiver<LogRecord>) {
    let (tx, rx) = crossbeam_channel::bounded::<LogRecord>(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (Sink::File { tx, dropped }, rx)
}

/// Spawns a background thread draining a bounded channel of `capacity`, formatting each record
/// and writing+flushing it to `writer` immediately (messages through this facility are expected
/// to be infrequent, so a per-line flush is not a throughput concern, and lets a UI tailing the
/// file see new lines right away). Returns a producer handle (backed by `Sink::File`) and a join
/// handle; once every `LogSender` clone is dropped, the channel disconnects, the thread drains
/// what's left and exits, and `join()` returns.
pub fn spawn_log_writer<W: Write + Send + 'static>(writer: W, capacity: usize) -> (LogSender, std::thread::JoinHandle<()>) {
    let (sink, rx) = bounded_file_sink(capacity);
    let handle = std::thread::spawn(move || {
        let mut writer = BufWriter::new(writer);
        let mut line = String::new();
        while let Ok(rec) = rx.recv() {
            line.clear();
            format_record(&rec, &mut line);
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }
    });
    (LogSender { sink, cycles: Default::default() }, handle)
}

/// Spawns a background thread draining a bounded channel of `capacity` and invoking `on_record`
/// for each record, so a host (e.g. the debugger) can consume structured `LogRecord`s directly
/// instead of only a formatted-text file. Returns a producer handle (backed by `Sink::File`,
/// same never-blocks/drop-and-count-on-full semantics as `spawn_log_writer`) and a join handle;
/// once every `LogSender` clone is dropped, the channel disconnects, the thread drains what's
/// left (calling `on_record` for each) and exits, and `join()` returns.
pub fn spawn_log_collector<F>(capacity: usize, mut on_record: F) -> (LogSender, std::thread::JoinHandle<()>)
where
    F: FnMut(LogRecord) + Send + 'static,
{
    let (sink, rx) = bounded_file_sink(capacity);
    let handle = std::thread::spawn(move || {
        while let Ok(rec) = rx.recv() {
            on_record(rec);
        }
    });
    (LogSender { sink, cycles: Default::default() }, handle)
}

/// Formats `$fmt, $args...` and logs it through `$sender` at `$level`/`$category` — the same
/// one-line ergonomics `debug!("...", ...)` already had at each of this pass's 11 call sites.
macro_rules! log_msg {
    ($sender:expr, $level:expr, $category:expr, $($arg:tt)*) => {
        $sender.log($level, $category, format!($($arg)*))
    };
}
pub(crate) use log_msg;

/// Builds a `LogSender`/`Receiver` pair for tests elsewhere in this crate (e.g. `Cpu` and device
/// `reset()` tests) to assert on the `LogRecord`s a call site emits, without a writer thread.
#[cfg(test)]
pub(crate) fn test_channel_sender(capacity: usize) -> (LogSender, crossbeam_channel::Receiver<LogRecord>) {
    let (sink, rx) = bounded_file_sink(capacity);
    (LogSender { sink, cycles: Default::default() }, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn sample_record(message: impl Into<String>) -> LogRecord {
        LogRecord {
            timestamp: std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_723_400_000_123),
            cycles: 42,
            level: LogLevel::Info,
            category: LogCategory::Device,
            message: message.into(),
        }
    }

    #[test]
    fn format_record_produces_expected_columns() {
        let rec = sample_record("via@0xc000 reset");
        let mut out = String::new();
        format_record(&rec, &mut out);
        let mut fields = out.trim_end_matches('\n').splitn(5, ' ');
        assert!(fields.next().unwrap().ends_with('Z'));
        assert_eq!(fields.next().unwrap(), "42");
        assert_eq!(fields.next().unwrap(), "INFO");
        assert_eq!(fields.next().unwrap(), "Device");
        assert_eq!(fields.next().unwrap(), "via@0xc000 reset");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn format_record_escapes_newlines_and_carriage_returns() {
        let rec = sample_record("line one\nline two\rcarriage");
        let mut out = String::new();
        format_record(&rec, &mut out);
        let message = out.trim_end_matches('\n').splitn(5, ' ').nth(4).unwrap();
        assert_eq!(message, "line one\\nline two\\rcarriage");
        // Only the trailing record-terminating '\n' appears literally.
        assert_eq!(out.matches('\n').count(), 1);
    }

    #[test]
    fn format_record_leaves_other_content_unescaped() {
        let rec = sample_record("no special chars here: 123 abc!");
        let mut out = String::new();
        format_record(&rec, &mut out);
        assert!(out.contains("no special chars here: 123 abc!"));
    }

    #[test]
    fn default_sender_forwards_through_log_crate_and_never_drops() {
        let sender = LogSender::default();
        sender.log(LogLevel::Info, LogCategory::Cpu, "hello");
        assert_eq!(sender.dropped_count(), 0);
    }

    #[test]
    fn full_channel_drops_and_counts_subsequent_records() {
        let (tx, rx) = crossbeam_channel::bounded::<LogRecord>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sender = LogSender { sink: Sink::File { tx, dropped: dropped.clone() }, cycles: Default::default() };

        sender.log(LogLevel::Info, LogCategory::Device, "first");
        sender.log(LogLevel::Info, LogCategory::Device, "second");

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(sender.dropped_count(), 1);
        let received = rx.recv().unwrap();
        assert_eq!(received.message, "first");
    }

    #[test]
    fn set_cycles_stamps_subsequent_records() {
        let (sender, rx) = test_channel_sender(4);

        sender.set_cycles(1000);
        sender.log(LogLevel::Info, LogCategory::Cpu, "tick");

        let received = rx.recv().unwrap();
        assert_eq!(received.cycles, 1000);
    }

    #[test]
    fn spawn_log_writer_flushes_and_closes_on_drop() {
        let buf: Vec<u8> = Vec::new();
        let cursor = SharedBuf::new(buf);
        let (sender, handle) = spawn_log_writer(cursor.clone(), 8);

        sender.log(LogLevel::Warn, LogCategory::Device, "shutting down");
        drop(sender);
        handle.join().unwrap();

        let contents = cursor.into_string();
        assert!(contents.contains("WARN"));
        assert!(contents.contains("shutting down"));
        assert!(contents.ends_with('\n'));
    }

    #[test]
    fn spawn_log_collector_invokes_callback_and_closes_on_drop() {
        let (tx, rx) = std::sync::mpsc::channel::<LogRecord>();
        let (sender, handle) = spawn_log_collector(8, move |rec| {
            let _ = tx.send(rec);
        });

        sender.log(LogLevel::Info, LogCategory::Cpu, "collected");
        drop(sender);
        handle.join().unwrap();

        let received: Vec<LogRecord> = rx.try_iter().collect();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].message, "collected");
        assert_eq!(received[0].level, LogLevel::Info);
        assert_eq!(received[0].category, LogCategory::Cpu);
    }

    #[test]
    fn log_msg_macro_formats_and_logs() {
        let (sender, rx) = test_channel_sender(4);

        log_msg!(sender, LogLevel::Error, LogCategory::Transport, "conn {} failed: {}", 3, "timeout");

        let received = rx.recv().unwrap();
        assert_eq!(received.message, "conn 3 failed: timeout");
        assert_eq!(received.level, LogLevel::Error);
        assert_eq!(received.category, LogCategory::Transport);
    }

    /// A `Write` sink shareable across threads for testing `spawn_log_writer`, backed by a
    /// `Mutex<Vec<u8>>` so the test thread can inspect what the writer thread wrote after joining.
    #[derive(Clone)]
    struct SharedBuf(Arc<std::sync::Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn new(buf: Vec<u8>) -> Self {
            SharedBuf(Arc::new(std::sync::Mutex::new(buf)))
        }

        fn into_string(self) -> String {
            let buf = Arc::try_unwrap(self.0).unwrap().into_inner().unwrap();
            String::from_utf8(buf).unwrap()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(data)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }
}
