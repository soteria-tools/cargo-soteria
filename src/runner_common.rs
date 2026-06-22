//! Logic shared between the two runners (the built-in parallel runner in
//! `base_runner.rs` and the cargo-nextest integration in `nextest.rs`):
//! building an environment-configured `soteria-rust` command, discovering a
//! crate's symbolic-test entry points, and the filter-escaping used to isolate a
//! single test.

use std::process::{Command, Stdio};

use crate::common::package_dir;

/// Build a [`Command`] for the bundled `soteria-rust`, with the environment it
/// needs to find its dynamic libraries and sibling tools (z3, obol, charon) and
/// pre-built plugins inside the install directory.
pub fn soteria_rust_command() -> Command {
    let pkg = package_dir();
    let bin_dir = pkg.join("bin");
    let lib_dir = pkg.join("lib");
    let plugins_dir = pkg.join("plugins");

    let lib_path_var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let existing_lib_path = std::env::var(lib_path_var).unwrap_or_default();
    let new_lib_path = if existing_lib_path.is_empty() {
        lib_dir.to_string_lossy().to_string()
    } else {
        format!("{}:{}", lib_dir.display(), existing_lib_path)
    };

    let mut cmd = Command::new(bin_dir.join("soteria-rust"));
    cmd.env(lib_path_var, &new_lib_path)
        .env("SOTERIA_Z3_PATH", bin_dir.join("z3"))
        .env("SOTERIA_OBOL_PATH", bin_dir.join("obol"))
        .env("SOTERIA_CHARON_PATH", bin_dir.join("charon"))
        .env("SOTERIA_RUST_PLUGINS", &plugins_dir);
    cmd
}

// ── test discovery ────────────────────────────────────────────────────────────

/// Why `discover_tests` couldn't produce a list, with enough captured context
/// to render a useful diagnostic via [`DiscoverError::message`].
pub enum DiscoverError {
    /// `soteria-rust` could not be spawned.
    Spawn(std::io::Error),
    /// Discovery ran but exited non-zero.
    Failed {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// Discovery exited 0 but stdout held no parseable test list.
    Unparseable { stdout: String, stderr: String },
}

impl DiscoverError {
    pub fn message(&self) -> String {
        match self {
            DiscoverError::Spawn(e) => format!("Failed to run soteria-rust: {e}"),
            DiscoverError::Failed {
                code,
                stdout,
                stderr,
            } => {
                // The compiler diagnostic soteria-rust emits (e.g. an E0308 type
                // error) lands on *stdout*; only the terse "Compiling… errored"
                // progress goes to stderr. Surface both so the user sees the
                // real error, not just our exit code.
                let mut msg = format!("Test discovery failed (exit {}).", code.unwrap_or(-1));
                for stream in [stderr.trim(), stdout.trim()] {
                    if !stream.is_empty() {
                        msg.push('\n');
                        msg.push_str(stream);
                    }
                }
                msg
            }
            DiscoverError::Unparseable { stdout, stderr } => format!(
                "Could not parse the test list from `soteria-rust compile --list-tests`.\n  stdout: {}\n  stderr: {}",
                stdout.trim(),
                stderr.trim(),
            ),
        }
    }
}

/// Compile the crate at the current directory once and list its symbolic-test
/// entry points, by running `soteria-rust compile --list-tests .` (forwarding
/// `passthrough`, so user flags like `--filter` are respected). Compilation
/// happens here exactly once; per-test runs then reuse the cached ULLBC via
/// `--no-compile`.
///
/// When `inherit_stderr` is set, compilation progress streams to our stderr
/// (the runner under nextest wants this, since its stdout must stay clean);
/// otherwise stderr is captured and surfaced only if discovery fails.
pub fn discover_tests(
    passthrough: &[String],
    test_target: Option<&str>,
    inherit_stderr: bool,
) -> Result<Vec<String>, DiscoverError> {
    let mut cmd = soteria_rust_command();
    cmd.arg("compile").arg("--list-tests").arg(".");
    if let Some(test_target) = test_target {
        cmd.arg("--test").arg(test_target);
    }
    cmd.args(passthrough).stdin(Stdio::null());
    if inherit_stderr {
        cmd.stderr(Stdio::inherit());
    }

    let output = cmd.output().map_err(DiscoverError::Spawn)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(list) = parse_test_list(&stdout) {
        return Ok(list);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(DiscoverError::Failed {
            code: output.status.code(),
            stdout: stdout.into_owned(),
            stderr,
        });
    }
    Err(DiscoverError::Unparseable {
        stdout: stdout.into_owned(),
        stderr,
    })
}

/// Parse the JSON array of test names. The whole stdout is normally one line;
/// be tolerant of stray output by falling back to the last line that parses.
pub fn parse_test_list(stdout: &str) -> Option<Vec<String>> {
    if let Ok(v) = serde_json::from_str::<Vec<String>>(stdout.trim()) {
        return Some(v);
    }
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.starts_with('[') {
            if let Ok(v) = serde_json::from_str::<Vec<String>>(line) {
                return Some(v);
            }
        }
    }
    None
}

/// Build an anchored, escaped `Str`-regex that matches exactly `name`.
/// soteria-rust's `--filter` is an OCaml `Str` substring regex, so without
/// anchoring `foo` would also select `foo_bar`.
pub fn anchored_filter(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 2);
    s.push('^');
    for c in name.chars() {
        // `Str` metacharacters that are special *unescaped*. Note `(` `)` `|`
        // `{` `}` are literal in `Str` (only special when backslash-escaped),
        // so escaping them would *change* the meaning — leave them alone.
        if matches!(c, '.' | '*' | '+' | '?' | '[' | ']' | '^' | '$' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('$');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_and_escapes() {
        assert_eq!(anchored_filter("a::b"), "^a::b$");
        // `.` `+` etc. get escaped so they match literally.
        assert_eq!(anchored_filter("m::a.b+c"), "^m::a\\.b\\+c$");
        // `(` `)` are literal in Str and must stay unescaped.
        assert_eq!(anchored_filter("f::g()"), "^f::g()$");
    }

    #[test]
    fn parses_test_list_json() {
        let v = parse_test_list("[\"a::x\",\"b::y\"]\n").unwrap();
        assert_eq!(v, vec!["a::x", "b::y"]);
        // tolerate a stray leading line
        let v = parse_test_list("warning: blah\n[\"only\"]\n").unwrap();
        assert_eq!(v, vec!["only"]);
        assert!(parse_test_list("not json").is_none());
    }
}
