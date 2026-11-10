//! Runs a planned compile on a background thread: one program after another, with their
//! output streamed into a shared log, progress read from the compilers' `0...1...2`
//! counters, errors and warnings collected, and a leak noticed from VBSP's pointfile.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use glam::DVec3;

use super::{Action, CommandLine, PlannedStep, StepKind};

/// Why VBSP reports a leak when it finds no entity inside the map. Stock VBSP only says
/// "leaked" then, and writes no leak line.
const NO_ENTITIES_INSIDE: &str = concat!(
    "The map leaked without a leak line, which means VBSP found no entity inside the map to start its ",
    "leak check from. It skips entities at exactly 0 0 0, so move the info_player_start (or any entity) ",
    "off the origin, even to 0 0 1, or add an entity inside the sealed area."
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Output,
    /// The command a step runs.
    Command,
    /// Mallet's own messages.
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

/// An error or warning found in the output, with what it points at in the map.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub step: usize,
    pub severity: Severity,
    pub text: String,
    /// The brush the line names, as VBSP's `Brush 12:` messages do with the solid id.
    pub brush: Option<u32>,
    /// A position the line gives, such as where an entity leaked.
    pub position: Option<DVec3>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Succeeded,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub step: Option<usize>,
    pub text: String,
    pub kind: LineKind,
}

#[derive(Debug, Clone)]
pub struct StepRun {
    pub title: String,
    pub kind: StepKind,
    pub command: String,
    pub status: StepStatus,
    pub started: Option<Instant>,
    pub elapsed: Option<Duration>,
    /// From the program's `0...10` counter, while it runs.
    pub progress: Option<f32>,
}

impl StepRun {
    /// Time spent so far, or in total once done.
    pub fn time(&self) -> Option<Duration> {
        self.elapsed.or_else(|| self.started.map(|started| started.elapsed()))
    }
}

/// Everything about a compile, shared between the runner and the window.
#[derive(Debug, Clone)]
pub struct JobState {
    pub map: String,
    pub preset: String,
    pub steps: Vec<StepRun>,
    pub lines: Vec<LogLine>,
    /// Lines kept in the log; older ones are dropped so a chatty compile cannot use up
    /// memory.
    pub max_lines: usize,
    pub problems: Vec<Problem>,
    pub started: Instant,
    pub started_at: SystemTime,
    pub finished: Option<Instant>,
    pub outcome: Option<Outcome>,
    /// Whether VBSP found a leak; `pointfile` then shows where.
    pub leaked: bool,
    /// Whether VBSP found no entity inside the map to start its leak check from, which
    /// it reports as a leak without a leak line.
    pub no_entities_inside: bool,
    pub pointfile: PathBuf,
    /// The text of the line the current program is still writing, such as `0...1...2`.
    pub partial: String,
    /// Set once the editor has reacted to the end of the compile.
    pub handled: bool,
}

impl JobState {
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    pub fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now) - self.started
    }

    /// Overall progress from 0 to 1: finished steps plus the running step's counter.
    pub fn progress(&self) -> f32 {
        if self.steps.is_empty() {
            return 1.0;
        }
        let done = self
            .steps
            .iter()
            .filter(|step| !matches!(step.status, StepStatus::Pending | StepStatus::Running))
            .count() as f32;
        let running = self
            .steps
            .iter()
            .find(|step| step.status == StepStatus::Running)
            .and_then(|step| step.progress)
            .unwrap_or(0.0);
        ((done + running) / self.steps.len() as f32).clamp(0.0, 1.0)
    }

    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.problems
            .iter()
            .filter(|problem| problem.severity == Severity::Warning)
            .count()
    }

    /// The whole log as text, for saving.
    pub fn log_text(&self) -> String {
        let mut text = format!("Mallet compile of {} with preset \"{}\"\n\n", self.map, self.preset);
        for line in &self.lines {
            let prefix = match line.kind {
                LineKind::Command => "> ",
                LineKind::Info => "-- ",
                _ => "",
            };
            text.push_str(prefix);
            text.push_str(&line.text);
            text.push('\n');
        }
        text
    }

    fn push(&mut self, step: Option<usize>, kind: LineKind, text: impl Into<String>) {
        let max_lines = self.max_lines.max(10);
        if self.lines.len() >= max_lines {
            self.lines.drain(..max_lines / 10);
        }
        self.lines.push(LogLine {
            step,
            text: text.into(),
            kind,
        });
    }

    /// Adds a line of program output, noting errors, warnings and leaks.
    fn output(&mut self, step: usize, text: &str) {
        let text = text.trim_end();
        let kind = classify(text);
        let lower = text.to_ascii_lowercase();
        if lower.contains("leaked") {
            self.leaked = true;
        }
        if lower.contains("no entities in open") {
            self.no_entities_inside = true;
        }
        if matches!(kind, LineKind::Error | LineKind::Warning) {
            self.problems.push(Problem {
                step,
                severity: if kind == LineKind::Error {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                text: text.trim().to_owned(),
                brush: number_after(text, "brush"),
                position: position_in(text),
            });
        }
        self.push(Some(step), kind, text);
    }
}

/// A running or finished compile.
pub struct CompileJob {
    state: Arc<Mutex<JobState>>,
    cancel: Arc<AtomicBool>,
}

impl CompileJob {
    /// Starts running `steps` on a background thread.
    /// Starts running `steps` on a background thread. `notes` are warnings found before
    /// compiling, listed first with the place they name. The log keeps the latest
    /// `max_lines` lines.
    pub fn start(
        map: String,
        preset: String,
        steps: Vec<PlannedStep>,
        pointfile: PathBuf,
        low_priority: bool,
        max_lines: usize,
        notes: Vec<(String, Option<DVec3>)>,
    ) -> Self {
        let state = JobState {
            map,
            preset,
            steps: steps
                .iter()
                .map(|step| StepRun {
                    title: step.title.clone(),
                    kind: step.kind,
                    command: step.action.display(),
                    status: StepStatus::Pending,
                    started: None,
                    elapsed: None,
                    progress: None,
                })
                .collect(),
            lines: Vec::new(),
            max_lines,
            problems: Vec::new(),
            started: Instant::now(),
            started_at: SystemTime::now(),
            finished: None,
            outcome: None,
            leaked: false,
            no_entities_inside: false,
            pointfile,
            partial: String::new(),
            handled: false,
        };
        let mut state = state;
        for (text, position) in notes {
            state.push(None, LineKind::Warning, text.clone());
            state.problems.push(Problem {
                step: 0,
                severity: Severity::Warning,
                text,
                brush: None,
                position,
            });
        }
        let state = Arc::new(Mutex::new(state));
        let cancel = Arc::new(AtomicBool::new(false));
        let (shared, cancelled) = (Arc::clone(&state), Arc::clone(&cancel));
        thread::Builder::new()
            .name("compile".to_owned())
            .spawn(move || run(&shared, &cancelled, &steps, low_priority))
            .expect("the compile thread starts");
        Self { state, cancel }
    }

    pub fn lock(&self) -> MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The shared state, for waiting on the compile without holding the editor.
    pub fn shared(&self) -> Arc<Mutex<JobState>> {
        Arc::clone(&self.state)
    }

    pub fn is_running(&self) -> bool {
        self.lock().is_running()
    }

    /// Stops the running program and skips the remaining steps.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

fn lock(state: &Mutex<JobState>) -> MutexGuard<'_, JobState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn run(state: &Mutex<JobState>, cancel: &AtomicBool, steps: &[PlannedStep], low_priority: bool) {
    let started_at = lock(state).started_at;
    let pointfile = lock(state).pointfile.clone();
    let mut failure: Option<String> = None;
    for (index, step) in steps.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        {
            let mut state = lock(state);
            state.steps[index].status = StepStatus::Running;
            state.steps[index].started = Some(Instant::now());
            state.push(Some(index), LineKind::Info, format!("{} started", step.title));
            state.push(Some(index), LineKind::Command, step.action.display());
        }
        let mut result = match &step.action {
            Action::Run(command) => run_program(state, cancel, index, command, low_priority),
            Action::Launch(command) => launch(command),
            Action::Copy { from, to } => copy(from, to),
        };
        // VBSP++ exits with an error on a leak and stock VBSP does not, so look for one
        // either way and explain it instead of the exit code.
        if step.kind == StepKind::Vbsp {
            let fresh_pointfile = fs::metadata(&pointfile)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified >= started_at);
            let mut state = lock(state);
            if fresh_pointfile {
                state.leaked = true;
            }
            if state.no_entities_inside || (state.leaked && !fresh_pointfile) {
                result = Err(NO_ENTITIES_INSIDE.to_owned());
            } else if state.leaked {
                result = Err(
                    "The map leaked: an entity inside can see the void. The red leak line in the views shows the way out."
                        .to_owned(),
                );
            }
        }
        let cancelled = cancel.load(Ordering::SeqCst);
        let mut state = lock(state);
        let run = &mut state.steps[index];
        run.elapsed = run.started.map(|started| started.elapsed());
        run.progress = None;
        let seconds = run.elapsed.unwrap_or_default().as_secs_f32();
        match result {
            Ok(message) => {
                run.status = StepStatus::Succeeded;
                if let Some(message) = message {
                    state.push(Some(index), LineKind::Info, message);
                }
                state.push(
                    Some(index),
                    LineKind::Info,
                    format!("{} finished in {seconds:.1} s", step.title),
                );
            }
            Err(_) if cancelled => {
                run.status = StepStatus::Cancelled;
                state.push(Some(index), LineKind::Info, format!("{} was cancelled", step.title));
            }
            Err(error) => {
                run.status = StepStatus::Failed;
                state.push(Some(index), LineKind::Error, error.clone());
                state.problems.push(Problem {
                    step: index,
                    severity: Severity::Error,
                    text: error.clone(),
                    brush: None,
                    position: None,
                });
                if !step.continue_on_error {
                    failure = Some(error);
                    break;
                }
            }
        }
        state.partial.clear();
    }
    let mut state = lock(state);
    let cancelled = cancel.load(Ordering::SeqCst);
    for run in &mut state.steps {
        if run.status == StepStatus::Pending {
            run.status = StepStatus::Skipped;
        }
    }
    let outcome = match failure {
        _ if cancelled => Outcome::Cancelled,
        Some(error) => Outcome::Failed(error),
        None => Outcome::Succeeded,
    };
    let seconds = state.elapsed().as_secs_f32();
    let summary = match &outcome {
        Outcome::Succeeded => format!("Compile finished in {seconds:.1} s"),
        Outcome::Failed(_) => format!("Compile failed after {seconds:.1} s"),
        Outcome::Cancelled => format!("Compile cancelled after {seconds:.1} s"),
    };
    state.push(None, LineKind::Info, summary);
    state.finished = Some(Instant::now());
    state.outcome = Some(outcome);
}

/// Runs a program, streaming its output into the log until it exits or is cancelled.
fn run_program(
    state: &Mutex<JobState>,
    cancel: &AtomicBool,
    index: usize,
    command: &CommandLine,
    low_priority: bool,
) -> Result<Option<String>, String> {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if command.working_dir.is_dir() {
        process.current_dir(&command.working_dir);
    }
    #[cfg(not(windows))]
    load_libraries_beside(&mut process, &command.program);
    hide_window(&mut process, low_priority);
    let mut child = process
        .spawn()
        .map_err(|error| format!("Could not start {}: {error}", command.program.display()))?;
    lower_priority(&child, low_priority);

    let (sender, receiver) = mpsc::channel::<(usize, Vec<u8>)>();
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ];
    for (stream, pipe) in pipes.into_iter().enumerate() {
        let Some(mut pipe) = pipe else { continue };
        let sender = sender.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = pipe.read(&mut buffer) {
                if read == 0 || sender.send((stream, buffer[..read].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    drop(sender);

    let mut partial = [String::new(), String::new()];
    let mut killed = false;
    loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok((stream, bytes)) => {
                let text = String::from_utf8_lossy(&bytes);
                partial[stream].push_str(&text);
                let mut state = lock(state);
                while let Some(end) = partial[stream].find('\n') {
                    let line: String = partial[stream].drain(..=end).collect();
                    // Progress counters rewrite the line with \r; keep the last version.
                    let line = line.trim_end_matches(['\n', '\r']);
                    let line = line.rsplit('\r').find(|part| !part.trim().is_empty()).unwrap_or("");
                    state.output(index, line);
                }
                let current = partial[stream].rsplit('\r').next().unwrap_or("").to_owned();
                if let Some(progress) = progress_of(&current) {
                    state.steps[index].progress = Some(progress);
                }
                state.partial = current;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::SeqCst) && !killed {
            let _ = child.kill();
            killed = true;
        }
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    {
        let mut state = lock(state);
        for rest in partial {
            if !rest.trim().is_empty() {
                state.output(index, &rest);
            }
        }
    }
    if killed {
        return Err("cancelled".to_owned());
    }
    if status.success() {
        Ok(None)
    } else {
        let name = command
            .program
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        Err(match status.code() {
            Some(code) => format!("{name} stopped with exit code {code}"),
            None => format!("{name} was stopped"),
        })
    }
}

#[cfg(windows)]
fn hide_window(process: &mut Command, low_priority: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    let priority = if low_priority { BELOW_NORMAL_PRIORITY_CLASS } else { 0 };
    process.creation_flags(CREATE_NO_WINDOW | priority);
}

/// Nothing to hide on Unix: the compile window spawns compilers without a
/// console of their own. Low priority is applied after spawn instead, on the
/// child itself, so cancelling still kills the compiler directly.
#[cfg(not(windows))]
fn hide_window(_process: &mut Command, _low_priority: bool) {}

/// Has a compile tool load the engine libraries that sit in its own folder, such as
/// `libtier0.so`, ahead of the folders `LD_LIBRARY_PATH` already lists. Without this the
/// Linux tools stop with a loader error before they start. Windows looks beside the
/// program by itself.
#[cfg(not(windows))]
fn load_libraries_beside(process: &mut Command, program: &Path) {
    let Some(folder) = program.parent().filter(|folder| !folder.as_os_str().is_empty()) else {
        return;
    };
    let existing = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
    let listed = std::env::split_paths(&existing).filter(|path| !path.as_os_str().is_empty());
    if let Ok(paths) = std::env::join_paths(std::iter::once(folder.to_path_buf()).chain(listed)) {
        process.env("LD_LIBRARY_PATH", paths);
    }
}

/// Runs the compiler at a lower priority when asked, as `BELOW_NORMAL` does on Windows.
#[cfg(not(windows))]
fn lower_priority(child: &std::process::Child, low_priority: bool) {
    if low_priority {
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, child.id(), 10);
        }
    }
}

/// Windows sets the priority when it starts the program, in `hide_window`.
#[cfg(windows)]
fn lower_priority(_child: &std::process::Child, _low_priority: bool) {}

/// Starts a program without waiting for it, such as the game.
fn launch(command: &CommandLine) -> Result<Option<String>, String> {
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    if command.working_dir.is_dir() {
        process.current_dir(&command.working_dir);
    }
    process
        .spawn()
        .map(|_| Some(format!("Started {}", command.program.display())))
        .map_err(|error| format!("Could not start {}: {error}", command.program.display()))
}

fn copy(from: &Path, to: &Path) -> Result<Option<String>, String> {
    if !from.is_file() {
        return Err(format!("{} does not exist; did VBSP run?", from.display()));
    }
    if let Some(folder) = to.parent() {
        fs::create_dir_all(folder).map_err(|error| format!("Could not create {}: {error}", folder.display()))?;
    }
    let bytes = fs::copy(from, to).map_err(|error| format!("Could not copy to {}: {error}", to.display()))?;
    Ok(Some(format!("Copied {:.1} MB to {}", bytes as f64 / 1e6, to.display())))
}

/// How a line of compiler output reads.
fn classify(text: &str) -> LineKind {
    let lower = text.to_ascii_lowercase();
    // Reports printed as JSON, such as `"warnings": [],`, name problems without having any.
    let trimmed = lower.trim();
    if trimmed.starts_with('"') && trimmed.contains("\":") {
        return LineKind::Output;
    }
    if lower.contains("0 warning") || lower.contains("no warning") {
        return LineKind::Output;
    }
    let error = lower.contains("leaked")
        || lower.starts_with("fatal")
        || lower.contains("fatal error")
        || (lower.contains("error") && !lower.contains("0 error") && !lower.contains("no error"));
    if error {
        LineKind::Error
    } else if lower.contains("warning") {
        LineKind::Warning
    } else {
        LineKind::Output
    }
}

/// The last step of a `0...1...2...3` counter, as a fraction of 10.
pub fn progress_of(text: &str) -> Option<f32> {
    if !text.contains("...") {
        return None;
    }
    text.split("...")
        .filter_map(|part| part.trim().parse::<u32>().ok())
        .filter(|number| *number <= 10)
        .last()
        .map(|number| number as f32 / 10.0)
}

/// The number after a word such as `brush`, as in `Brush 1234: plane with no normal`.
fn number_after(text: &str, word: &str) -> Option<u32> {
    let lower = text.to_ascii_lowercase();
    let mut search = 0;
    while let Some(found) = lower[search..].find(word) {
        let after = search + found + word.len();
        let rest = lower[after..].trim_start_matches([' ', ':', '#']);
        let rest = rest
            .strip_prefix("id")
            .unwrap_or(rest)
            .trim_start_matches([' ', ':', '#']);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(number) = digits.parse() {
            return Some(number);
        }
        search = after;
    }
    None
}

/// A position in parentheses, as in `Entity 12 (-128.00 64.00 0.00) leaked!`.
fn position_in(text: &str) -> Option<DVec3> {
    let start = text.find('(')?;
    let end = start + text[start..].find(')')?;
    let numbers: Vec<f64> = text[start + 1..end]
        .split([' ', ','])
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers.as_slice() {
        [x, y, z] => Some(DVec3::new(*x, *y, *z)),
        _ => None,
    }
}

/// Reads a VBSP pointfile (`.lin`): one point per line, tracing the leak from an entity
/// to the void.
pub fn read_pointfile(path: &Path) -> Option<Vec<DVec3>> {
    let text = fs::read_to_string(path).ok()?;
    let points: Vec<DVec3> = text
        .lines()
        .filter_map(|line| {
            let numbers: Vec<f64> = line.split_whitespace().filter_map(|part| part.parse().ok()).collect();
            (numbers.len() == 3).then(|| DVec3::new(numbers[0], numbers[1], numbers[2]))
        })
        .collect();
    (points.len() >= 2).then_some(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_progress_counters() {
        assert_eq!(progress_of("0...1...2...3"), Some(0.3));
        assert_eq!(
            progress_of("Building visibility clusters...0...1...2...3...4...5...6...7...8...9...10"),
            Some(1.0)
        );
        assert_eq!(progress_of("Building Physics collision data..."), None);
        assert_eq!(progress_of("Writing map.bsp"), None);
    }

    #[test]
    fn finds_problems_and_what_they_point_at() {
        assert_eq!(classify("Brush 1234: WARNING, microbrush"), LineKind::Warning);
        assert_eq!(classify("**** leaked ****"), LineKind::Error);
        assert_eq!(
            classify("Error loading studio model \"models/x.mdl\"!"),
            LineKind::Error
        );
        assert_eq!(classify("0 errors"), LineKind::Output);
        assert_eq!(classify("  \"warnings\": [],"), LineKind::Output);
        assert_eq!(classify("Finished with 0 warnings"), LineKind::Output);
        assert_eq!(number_after("Brush 1234: WARNING, microbrush", "brush"), Some(1234));
        assert_eq!(number_after("Brush id 77: plane with no normal", "brush"), Some(77));
        assert_eq!(
            position_in("Entity info_player_start (-128.00 64.00 0.00) leaked!"),
            Some(DVec3::new(-128.0, 64.0, 0.0))
        );
    }

    #[test]
    fn runs_programs_and_stops_on_failure() {
        let shell = |script: &str| CommandLine {
            program: PathBuf::from("cmd"),
            args: vec!["/C".to_owned(), script.to_owned()],
            working_dir: std::env::temp_dir(),
        };
        if !cfg!(windows) {
            return;
        }
        let step = |title: &str, script: &str| PlannedStep {
            title: title.to_owned(),
            kind: StepKind::Custom,
            action: Action::Run(shell(script)),
            continue_on_error: false,
        };
        let steps = vec![
            step("first", "echo 0...1...2 && echo Brush 5: warning, tiny"),
            step("second", "exit 3"),
            step("third", "echo never"),
        ];
        let job = CompileJob::start(
            "test".to_owned(),
            "p".to_owned(),
            steps,
            PathBuf::from("none.lin"),
            false,
            1000,
            vec![("checked first".to_owned(), None)],
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while job.is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let state = job.lock();
        assert!(matches!(state.outcome, Some(Outcome::Failed(_))), "{:?}", state.outcome);
        let statuses: Vec<StepStatus> = state.steps.iter().map(|step| step.status).collect();
        assert_eq!(
            statuses,
            [StepStatus::Succeeded, StepStatus::Failed, StepStatus::Skipped]
        );
        assert_eq!(state.warnings(), 2);
        assert_eq!(state.problems[0].text, "checked first");
        assert_eq!(state.problems[1].brush, Some(5));
        assert!(state.lines.iter().any(|line| line.text.contains("exit code 3")));
    }
}
