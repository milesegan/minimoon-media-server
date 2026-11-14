//! The CLI's terminal output: a progress line at the bottom of the terminal, with messages and
//! log lines printed above it.

use super::{format_count, paint, ANSI_BOLD, ANSI_CYAN, ANSI_DIM, ANSI_GREEN};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Moves to the start of the line and erases it.
const CLEAR_LINE: &str = "\r\x1b[2K";
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const MAX_BAR_WIDTH: usize = 30;
const MIN_BAR_WIDTH: usize = 10;
/// How long a rate is measured before an estimate of the time left is shown.
const ETA_AFTER: Duration = Duration::from_secs(3);

/// Whether a progress line is shown at all: only on a terminal.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The progress line currently on screen.
static LINE: Mutex<Option<String>> = Mutex::new(None);

fn line() -> MutexGuard<'static, Option<String>> {
    LINE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Turns the progress line on, when stdout is a terminal.
pub fn enable_progress(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Prints a line above the progress line.
pub fn println(text: impl AsRef<str>) {
    write_above(format!("{}\n", text.as_ref()).as_bytes());
}

fn write_above(bytes: &[u8]) {
    let line = line();
    let mut out = std::io::stdout().lock();
    if line.is_some() {
        let _ = out.write_all(CLEAR_LINE.as_bytes());
    }
    let _ = out.write_all(bytes);
    if let Some(line) = &*line {
        let _ = out.write_all(line.as_bytes());
    }
    let _ = out.flush();
}

/// Shows `text` as the progress line, or removes the progress line.
pub fn set_progress(text: Option<String>) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let mut line = line();
    if *line == text {
        return;
    }
    let mut out = std::io::stdout().lock();
    if line.is_some() {
        let _ = out.write_all(CLEAR_LINE.as_bytes());
    }
    if let Some(text) = &text {
        let _ = out.write_all(text.as_bytes());
    }
    let _ = out.flush();
    *line = text;
}

/// A `tracing` writer that prints each log line above the progress line.
#[derive(Default)]
pub struct LogWriter(Vec<u8>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            write_above(&self.0);
        }
    }
}

/// The terminal's width in columns.
pub fn width() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ only writes a `winsize` into the struct it's given.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
            && size.ws_col > 0
        {
            return usize::from(size.ws_col);
        }
    }
    80
}

/// What the progress line shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub label: String,
    /// Files done and the total, or `None` for a spinner alone.
    pub progress: Option<(usize, usize)>,
}

/// Draws a [`Task`] as one line: a spinner, the label, a bar, counts and the time left.
#[derive(Default)]
pub struct ProgressLine {
    frame: usize,
    /// Where the current rate measurement started: the task's label and total, when, and how
    /// many files were done then.
    rate_start: Option<(String, usize, Instant, usize)>,
}

/// A piece of the line, plain (for measuring) and as printed.
struct Segment {
    plain: String,
    painted: String,
}

impl Segment {
    fn new(text: String, code: &str, color: bool) -> Self {
        Self {
            painted: paint(&text, code, color),
            plain: text,
        }
    }

    fn len(&self) -> usize {
        self.plain.chars().count()
    }
}

impl ProgressLine {
    pub fn render(&mut self, task: &Task, now: Instant, width: usize, color: bool) -> String {
        let spinner = SPINNER[self.frame % SPINNER.len()];
        self.frame += 1;
        // Leave the last column free: writing into it wraps on some terminals.
        let width = width.saturating_sub(1);
        let head_text = format!("{spinner} {}", task.label);
        let Some((done, total)) = task.progress else {
            self.rate_start = None;
            return paint_head(&truncate(&head_text, width), color);
        };
        let done = done.min(total);
        let eta = self.eta(task, done, total, now);
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        let percent = Segment::new(format!("{percent:>3}%"), ANSI_BOLD, color);
        let counts = Segment::new(
            format!("{} / {} files", format_count(done), format_count(total)),
            ANSI_DIM,
            color,
        );
        let eta = eta.map(|eta| Segment::new(eta, ANSI_DIM, color));
        let head_len = head_text.chars().count();
        // Drop the time left, then the counts, before the bar gets too narrow.
        let layouts: [(bool, bool); 3] = [(true, true), (true, false), (false, false)];
        for (with_counts, with_eta) in layouts {
            let mut tail: Vec<&Segment> = vec![&percent];
            if with_counts {
                tail.push(&counts);
            }
            if with_eta {
                match &eta {
                    Some(eta) => tail.push(eta),
                    None => continue,
                }
            }
            let tail_len: usize = tail.iter().map(|s| s.len() + 2).sum();
            let room = width.saturating_sub(head_len + 2 + tail_len);
            if room < MIN_BAR_WIDTH {
                continue;
            }
            let bar = bar(done, total, room.min(MAX_BAR_WIDTH), color);
            let tail: Vec<&str> = tail.iter().map(|s| s.painted.as_str()).collect();
            return format!(
                "{}  {bar}  {}",
                paint_head(&head_text, color),
                tail.join("  ")
            );
        }
        let head = truncate(&head_text, width.saturating_sub(percent.len() + 2));
        format!("{}  {}", paint_head(&head, color), percent.painted)
    }

    /// The time left, from the rate since this task started, once it has run long enough to
    /// measure.
    fn eta(&mut self, task: &Task, done: usize, total: usize, now: Instant) -> Option<String> {
        let restart = match &self.rate_start {
            Some((label, start_total, _, start_done)) => {
                *label != task.label || *start_total != total || done < *start_done
            }
            None => true,
        };
        if restart {
            self.rate_start = Some((task.label.clone(), total, now, done));
            return None;
        }
        let (_, _, started, start_done) = self.rate_start.as_ref()?;
        let elapsed = now.duration_since(*started);
        let progressed = done - start_done;
        if elapsed < ETA_AFTER || progressed == 0 {
            return None;
        }
        let left = elapsed.as_secs_f64() * (total - done) as f64 / progressed as f64;
        Some(format!("{} left", format_duration(left.round() as u64)))
    }
}

fn paint_head(head: &str, color: bool) -> String {
    let mut chars = head.chars();
    let Some(spinner) = chars.next() else {
        return String::new();
    };
    format!(
        "{}{}",
        paint(spinner.to_string(), ANSI_CYAN, color),
        paint(chars.as_str(), ANSI_BOLD, color)
    )
}

fn bar(done: usize, total: usize, width: usize, color: bool) -> String {
    let filled = (done * width).checked_div(total).unwrap_or(width);
    if color {
        format!(
            "{}{}",
            paint("━".repeat(filled), ANSI_GREEN, true),
            paint("━".repeat(width - filled), ANSI_DIM, true)
        )
    } else {
        format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(width.saturating_sub(1)).collect();
    truncated.truncate(truncated.trim_end().len());
    truncated.push('…');
    truncated
}

/// `42s`, `3:07` or `1:02:03`.
fn format_duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else if minutes > 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(label: &str, progress: Option<(usize, usize)>) -> Task {
        Task {
            label: label.into(),
            progress,
        }
    }

    #[test]
    fn draws_a_bar_with_counts_and_then_the_time_left() {
        let mut line = ProgressLine::default();
        let start = Instant::now();
        let first = line.render(&task("Indexing Music", Some((0, 1000))), start, 100, false);
        assert_eq!(
            first,
            format!(
                "⠋ Indexing Music  {}    0%  0 / 1,000 files",
                "░".repeat(MAX_BAR_WIDTH)
            )
        );
        let later = line.render(
            &task("Indexing Music", Some((250, 1000))),
            start + Duration::from_secs(10),
            100,
            false,
        );
        assert_eq!(
            later,
            format!(
                "⠙ Indexing Music  {}{}   25%  250 / 1,000 files  30s left",
                "█".repeat(7),
                "░".repeat(23)
            )
        );
    }

    #[test]
    fn drops_details_to_fit_narrow_terminals() {
        let mut line = ProgressLine::default();
        let narrow = line.render(
            &task("Indexing Music", Some((500, 1000))),
            Instant::now(),
            40,
            false,
        );
        assert_eq!(
            narrow,
            format!("⠋ Indexing Music  {}   50%", "█".repeat(7) + &"░".repeat(8))
        );
        assert!(narrow.chars().count() < 40);
        let tiny = line.render(
            &task("Indexing a long folder name", Some((1, 2))),
            Instant::now(),
            20,
            false,
        );
        assert!(tiny.chars().count() < 20, "{tiny}");
        assert!(tiny.ends_with(" 50%"));
    }

    #[test]
    fn a_task_without_counts_is_a_spinner() {
        let mut line = ProgressLine::default();
        let text = line.render(
            &task("Listing files in Music", None),
            Instant::now(),
            80,
            false,
        );
        assert_eq!(text, "⠋ Listing files in Music");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(42), "42s");
        assert_eq!(format_duration(187), "3:07");
        assert_eq!(format_duration(3723), "1:02:03");
    }
}
