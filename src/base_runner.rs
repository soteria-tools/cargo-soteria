//! Parallel test runner for `cargo soteria`.
//!
//! Running `cargo soteria` (with no management subcommand) discovers the
//! crate's symbolic-test entry points and runs each one in its own process,
//! spread across a pool of worker threads. Results stream to the terminal as
//! each test finishes.
//!
//! Pipeline:
//!   1. **Discover** — `soteria-rust compile --list-tests .` compiles the crate
//!      once and prints the entry points as a one-line JSON array on stdout
//!      (compilation progress goes to stderr).
//!   2. **Fan out** — a worker pool runs, per test,
//!      `soteria-rust exec . --no-compile --no-compile-plugins --filter ^test$`.
//!
//! Design notes:
//!   * Compilation happens exactly once, in step 1. Workers pass `--no-compile`
//!     so they reuse the cached ULLBC instead of each re-invoking cargo/charon,
//!     which would race on the crate's shared target directory.
//!   * Each worker isolates one entry point with an *anchored, escaped*
//!     `--filter` regex (`^name$`), so it runs exactly one test even when one
//!     test name is a substring of another (`--filter` is a substring regex).
//!   * Every child is spawned in its own process group; on Ctrl-C we SIGKILL
//!     each group, so no soteria-rust — nor its z3/charon grandchildren —
//!     survives the interrupt.
//!   * A single soteria-rust crash (exit 2/3, or a fatal signal) is recorded
//!     for that test; the remaining tests keep running.

use std::collections::HashSet;
use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::common::{fail, ok, spinner, warn};
use crate::runner_common::{self, anchored_filter, soteria_rust_command};

/// Default worker count: a quarter of the available parallelism (at least 1).
pub fn default_jobs() -> usize {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (n / 4).max(1)
}

// ── result model ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Failed,
    Crashed,
    Skipped,
    Error,
}

/// What a single `exec` invocation produced.
struct RunOutcome {
    status: Status,
    /// Short human note, e.g. "issues found" or "soteria crashed (exit 2)".
    detail: String,
    /// Captured, merged stdout+stderr of the run.
    output: String,
}

impl RunOutcome {
    fn error(msg: String) -> Self {
        RunOutcome {
            status: Status::Error,
            detail: msg,
            output: String::new(),
        }
    }
}

struct TestResult {
    name: String,
    status: Status,
    detail: String,
    output: String,
    duration: Duration,
}

#[derive(Default)]
struct Counts {
    passed: usize,
    failed: usize,
    crashed: usize,
    skipped: usize,
    errored: usize,
}

impl Counts {
    fn tally(&mut self, s: Status) {
        match s {
            Status::Passed => self.passed += 1,
            Status::Failed => self.failed += 1,
            Status::Crashed => self.crashed += 1,
            Status::Skipped => self.skipped += 1,
            Status::Error => self.errored += 1,
        }
    }

    fn done(&self) -> usize {
        self.passed + self.failed + self.crashed + self.skipped + self.errored
    }

    /// Compact one-line tally for the live progress bar.
    fn compact(&self) -> String {
        let mut parts = Vec::new();
        if self.passed > 0 {
            parts.push(format!("{} {}", "✓".green(), self.passed));
        }
        if self.failed > 0 {
            parts.push(format!("{} {}", "✗".red(), self.failed));
        }
        if self.crashed > 0 {
            parts.push(format!("{} {}", "💥", self.crashed));
        }
        if self.errored > 0 {
            parts.push(format!("{} {}", "⚠".yellow(), self.errored));
        }
        parts.join("  ")
    }
}

// ── public entry point ──────────────────────────────────────────────────────

/// Discover and run all symbolic tests in the crate at the current directory,
/// `jobs` at a time. Diverges: always exits the process.
///
/// Exit code: `0` all passed · `1` some failed · `2` some crashed/errored ·
/// `130` interrupted.
pub fn run(passthrough: Vec<String>, jobs: usize) -> ! {
    // A single worker gains nothing from discovery + fan-out: hand off to
    // `soteria-rust exec . [args]`, which already runs every test itself on one
    // thread, streaming its own output directly. (`-j 1`, or the default on
    // machines with few CPUs.)
    if jobs == 1 {
        run_serial(&passthrough);
    }

    let tests = discover_tests(&passthrough);
    let total = tests.len();
    if total == 0 {
        ok("No tests found.");
        std::process::exit(0);
    }

    let jobs = jobs.clamp(1, total);
    let exec_args = Arc::new(strip_filter_exclude(&passthrough));
    let tests = Arc::new(tests);
    let tty = std::io::stderr().is_terminal();

    let interrupted = Arc::new(AtomicBool::new(false));
    let registry: Arc<Mutex<HashSet<i32>>> = Arc::new(Mutex::new(HashSet::new()));
    install_interrupt_handler(interrupted.clone(), registry.clone());

    print_header(total, jobs);

    // ── live UI ──────────────────────────────────────────────────────────────
    let multi = MultiProgress::new();
    if !tty {
        // Piped/non-interactive: don't draw bars, just stream result lines.
        multi.set_draw_target(ProgressDrawTarget::hidden());
    }
    let worker_bars: Vec<ProgressBar> = (0..jobs)
        .map(|_| {
            let pb = multi.add(ProgressBar::new_spinner());
            pb.set_style(worker_style());
            pb.set_message("idle".dimmed().to_string());
            if tty {
                pb.enable_steady_tick(Duration::from_millis(90));
            }
            pb
        })
        .collect();
    let main_bar = multi.add(ProgressBar::new(total as u64));
    main_bar.set_style(main_style());
    if tty {
        main_bar.enable_steady_tick(Duration::from_millis(90));
    }

    // ── worker pool ────────────────────────────────────────────────────────
    // A lock-free cursor hands the next test to whichever worker is free, so
    // long-running tests don't stall the others.
    let cursor = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel::<TestResult>();
    let start = Instant::now();

    let mut handles = Vec::with_capacity(jobs);
    for bar in &worker_bars {
        let tests = tests.clone();
        let exec_args = exec_args.clone();
        let cursor = cursor.clone();
        let interrupted = interrupted.clone();
        let registry = registry.clone();
        let tx = tx.clone();
        let bar = bar.clone();
        handles.push(std::thread::spawn(move || {
            while !interrupted.load(Ordering::SeqCst) {
                let i = cursor.fetch_add(1, Ordering::SeqCst);
                if i >= tests.len() {
                    break;
                }
                let test = &tests[i];
                bar.reset_elapsed();
                bar.set_message(test.clone());
                let t0 = Instant::now();
                let outcome = run_one(test, &exec_args, tty, &registry, &interrupted);
                bar.set_message("idle".dimmed().to_string());
                let result = TestResult {
                    name: test.clone(),
                    status: outcome.status,
                    detail: outcome.detail,
                    output: outcome.output,
                    duration: t0.elapsed(),
                };
                if tx.send(result).is_err() {
                    break;
                }
            }
            bar.finish_and_clear();
        }));
    }
    drop(tx); // so `rx` ends once every worker has finished

    // ── collect + report as results arrive ──────────────────────────────────
    let mut counts = Counts::default();
    let mut failures: Vec<TestResult> = Vec::new();
    for r in rx {
        counts.tally(r.status);
        let block = format_result(&r);
        if tty {
            let _ = multi.println(block);
        } else {
            println!("{block}");
        }
        main_bar.set_position(counts.done() as u64);
        main_bar.set_message(counts.compact());
        if matches!(r.status, Status::Failed | Status::Crashed | Status::Error) {
            failures.push(r);
        }
    }
    for h in handles {
        let _ = h.join();
    }
    main_bar.finish_and_clear();
    for b in &worker_bars {
        b.finish_and_clear();
    }

    let was_interrupted = interrupted.load(Ordering::SeqCst);
    print_summary(&counts, &failures, total, start.elapsed(), was_interrupted);

    let code = if was_interrupted {
        130
    } else if counts.crashed > 0 || counts.errored > 0 {
        2
    } else if counts.failed > 0 {
        1
    } else {
        0
    };
    std::process::exit(code);
}

/// `-j 1`: skip discovery and fan-out — run `soteria-rust exec . [args]`, which
/// analyses every test on a single thread and streams its own output directly.
/// Diverges.
fn run_serial(passthrough: &[String]) -> ! {
    let status = soteria_rust_command()
        .arg("exec")
        .arg(".")
        .args(passthrough)
        .status();
    match status {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => fail(&format!("Failed to execute soteria-rust: {e}")),
    }
}

// ── discovery ────────────────────────────────────────────────────────────────

fn discover_tests(passthrough: &[String]) -> Vec<String> {
    let sp = spinner("Discovering tests…");
    let result = runner_common::discover_tests(
        passthrough,
        // The built-in runner analyses the lib target only; integration-test
        // targets are supported through nextest (see `nextest.rs`).
        None,
        false,
    );
    sp.finish_and_clear();
    result.unwrap_or_else(|e| fail(&e.message()))
}

// ── single test execution ────────────────────────────────────────────────────

fn run_one(
    test: &str,
    exec_args: &[String],
    tty: bool,
    registry: &Mutex<HashSet<i32>>,
    interrupted: &AtomicBool,
) -> RunOutcome {
    let mut cmd = soteria_rust_command();
    cmd.arg("exec")
        .arg(".")
        .arg("--no-compile")
        .arg("--no-compile-plugins")
        .args(exec_args)
        .arg("--filter")
        .arg(anchored_filter(test))
        .stdin(Stdio::null())
        // New process group so a Ctrl-C can SIGKILL the whole subtree at once.
        .process_group(0);
    if !tty {
        // Keep piped output clean; in a terminal we keep soteria's colours.
        cmd.env("NO_COLOR", "1");
    }

    // Merge stdout+stderr into a single temp file by handing both streams the
    // same (dup'd) descriptor, so the captured output preserves ordering and we
    // never risk a two-pipe read deadlock.
    let mut file = match tempfile::tempfile() {
        Ok(f) => f,
        Err(e) => return RunOutcome::error(format!("could not create temp file: {e}")),
    };
    let (out_h, err_h) = match (file.try_clone(), file.try_clone()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return RunOutcome::error("could not duplicate temp file handle".into()),
    };
    cmd.stdout(Stdio::from(out_h)).stderr(Stdio::from(err_h));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOutcome::error(format!("could not spawn soteria-rust: {e}")),
    };
    let pid = child.id() as i32;
    if let Ok(mut g) = registry.lock() {
        g.insert(pid);
    }
    // Cover the race where Ctrl-C fired between spawn and registration: the
    // handler may have already taken its snapshot, so kill ourselves now.
    if interrupted.load(Ordering::SeqCst) {
        unsafe { libc::killpg(pid, libc::SIGKILL) };
    }

    let status = child.wait();

    if let Ok(mut g) = registry.lock() {
        g.remove(&pid);
    }

    let mut output = String::new();
    // A read failure here is very unlikely (a local temp file we just wrote),
    // but if it happens, make it visible rather than passing empty output to
    // `classify`. (Otherwise, we would misread an exit-0 pass as "no test
    // matched filter" or drop a failing test's diagnostic.)
    if let Err(e) = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.read_to_string(&mut output))
    {
        output.push_str(&format!(
            "\n[cargo-soteria: could not read captured output: {e}]"
        ));
    }

    match status {
        Ok(st) => classify(st.code(), st.signal(), &output, interrupted),
        Err(e) => RunOutcome::error(format!("could not wait for soteria-rust: {e}")),
    }
}

/// Map an `exec` exit status to a result. Exit codes follow soteria-rust:
/// `0` success · `1` bug/error found · `2` soteria crash · `3` charon crash.
fn classify(
    code: Option<i32>,
    signal: Option<i32>,
    output: &str,
    interrupted: &AtomicBool,
) -> RunOutcome {
    let (status, detail) = match code {
        Some(0) if output.contains("Running") => (Status::Passed, "no issues".to_string()),
        // Exit 0 but nothing ran means the anchored filter matched no entry
        // point — surface it instead of silently counting a pass.
        Some(0) => (Status::Error, "no test matched filter".to_string()),
        Some(1) => (Status::Failed, "issues found".to_string()),
        Some(2) => (Status::Crashed, "soteria crashed (exit 2)".to_string()),
        Some(3) => (Status::Crashed, "charon crashed (exit 3)".to_string()),
        Some(c) => (Status::Crashed, format!("unexpected exit {c}")),
        None => {
            if interrupted.load(Ordering::SeqCst) {
                (Status::Skipped, "interrupted".to_string())
            } else {
                (
                    Status::Crashed,
                    format!("killed by signal {}", signal.unwrap_or(0)),
                )
            }
        }
    };
    RunOutcome {
        status,
        detail,
        output: output.to_string(),
    }
}

/// Drop the user's `--filter`/`--exclude` (and their values) from the args
/// forwarded to each worker's `exec`: the discovered list already reflects
/// them, and each worker supplies its own anchored `--filter`. Leaving the
/// user's `--filter` in would union with ours and run *every* matching test per
/// worker.
fn strip_filter_exclude(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--filter" || a == "--exclude" {
            i += 2; // skip flag and its separate value
            continue;
        }
        if a.starts_with("--filter=") || a.starts_with("--exclude=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

// ── interrupt handling ────────────────────────────────────────────────────────

fn install_interrupt_handler(interrupted: Arc<AtomicBool>, registry: Arc<Mutex<HashSet<i32>>>) {
    // ctrlc runs this on a dedicated thread, so normal Rust is safe here.
    let res = ctrlc::set_handler(move || {
        interrupted.store(true, Ordering::SeqCst);
        let pids: Vec<i32> = registry
            .lock()
            .map(|g| g.iter().copied().collect())
            .unwrap_or_default();
        for pid in pids {
            // Negative-pid / killpg: SIGKILL the worker's whole process group,
            // taking down soteria-rust and any z3/charon it spawned.
            unsafe { libc::killpg(pid, libc::SIGKILL) };
        }
    });
    // If a handler can't be installed we still run; only the kill-on-Ctrl-C
    // guarantee is lost, so it's not worth aborting over.
    let _ = res;
}

// ── presentation ──────────────────────────────────────────────────────────────

fn worker_style() -> ProgressStyle {
    ProgressStyle::with_template("    {spinner:.cyan} {wide_msg} {elapsed:>4}")
        .unwrap()
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "·"])
}

fn main_style() -> ProgressStyle {
    ProgressStyle::with_template("    {bar:28.cyan/dim} {pos}/{len}  {msg}  ({elapsed_precise})")
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  ")
}

fn print_header(total: usize, jobs: usize) {
    println!();
    println!(
        "  {} {} {} · {} {}",
        "Soteria".bold().cyan(),
        "running".dimmed(),
        format!("{total} {}", plural(total, "test", "tests")).bold(),
        format!("{jobs}").bold(),
        plural(jobs, "worker", "workers"),
    );
    println!();
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

fn format_result(r: &TestResult) -> String {
    let time = format!("{:.2}s", r.duration.as_secs_f64());
    let (icon, name): (colored::ColoredString, colored::ColoredString) = match r.status {
        Status::Passed => ("✓".green().bold(), r.name.as_str().normal()),
        Status::Failed => ("✗".red().bold(), r.name.as_str().red()),
        Status::Crashed => ("💥".normal(), r.name.as_str().red().bold()),
        Status::Skipped => ("⏭".dimmed(), r.name.as_str().dimmed()),
        Status::Error => ("⚠".yellow().bold(), r.name.as_str().yellow()),
    };

    let mut s = match r.status {
        Status::Passed => format!("  {icon} {name}  {}", time.dimmed()),
        Status::Skipped => format!("  {icon} {name}  {}", r.detail.dimmed()),
        _ => format!(
            "  {icon} {name}  {}  {}",
            time.dimmed(),
            format!("— {}", r.detail).dimmed()
        ),
    };

    // Attach the captured output for anything that isn't a clean pass/skip, so
    // the diagnostic streams out with the result.
    if matches!(r.status, Status::Failed | Status::Crashed | Status::Error) {
        let trimmed = r.output.trim_end();
        if !trimmed.is_empty() {
            for line in trimmed.lines() {
                s.push('\n');
                s.push_str("      ");
                s.push_str(line);
            }
        }
    }
    s
}

fn print_summary(
    counts: &Counts,
    failures: &[TestResult],
    total: usize,
    elapsed: Duration,
    interrupted: bool,
) {
    println!();
    println!("  {}", "── Summary ──────────────────────────".dimmed());

    let mut line = vec![format!("{} passed", counts.passed)
        .green()
        .bold()
        .to_string()];
    if counts.failed > 0 {
        line.push(format!("{} failed", counts.failed).red().bold().to_string());
    }
    if counts.crashed > 0 {
        line.push(
            format!("{} crashed", counts.crashed)
                .red()
                .bold()
                .to_string(),
        );
    }
    if counts.errored > 0 {
        line.push(
            format!("{} errored", counts.errored)
                .yellow()
                .bold()
                .to_string(),
        );
    }
    if counts.skipped > 0 {
        line.push(format!("{} skipped", counts.skipped).dimmed().to_string());
    }
    println!(
        "  {}   {}",
        line.join("   "),
        format!("in {:.1}s", elapsed.as_secs_f64()).dimmed()
    );

    let not_run = total.saturating_sub(counts.done());
    if interrupted {
        warn(&format!("Interrupted — {not_run} test(s) not run."));
    } else if not_run > 0 {
        warn(&format!("{not_run} test(s) not run."));
    }

    if !failures.is_empty() {
        println!();
        println!("  {}", "Failing tests:".bold());
        for f in failures {
            let tag = match f.status {
                Status::Crashed => "crashed".red(),
                Status::Error => "error".yellow(),
                _ => "failed".red(),
            };
            println!("    {} {}  {}", "•".dimmed(), f.name.as_str().red(), tag);
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_user_filter_and_exclude() {
        let args = vec![
            "--kani".to_string(),
            "--filter".to_string(),
            "foo".to_string(),
            "--summary".to_string(),
            "--exclude=bar".to_string(),
            "--test".to_string(),
            "lib".to_string(),
        ];
        assert_eq!(
            strip_filter_exclude(&args),
            vec!["--kani", "--summary", "--test", "lib"]
        );
    }
}
