// src/launch_split_term_module.rs

//! # launch_split_term_module
//!
//! ## Purpose
//!
//! A small, focused helper module for applications that need to launch
//! a child process in one of three ways:
//!
//!   1. In a freshly-opened terminal emulator window (e.g. on a desktop OS).
//!   2. In a new tmux pane created by a vertical split of the current pane.
//!   3. In a new tmux pane created by a horizontal split of the current pane.
//!
//! A fourth case — "launch in the same terminal" — is trivial for the
//! caller (it just runs the child inline) and is intentionally not
//! provided here. Adding a no-op stub for that case would add API
//! surface for zero benefit.
//!
//! ## What This Module Is Not
//!
//! - It is not application-specific. It has no knowledge of any
//!   particular caller's command-line flags or business logic.
//! - It is not a command-line argument parser. The caller decides what
//!   flags to pass to the child; this module forwards them as-is.
//! - It does not choose what to launch. The caller supplies the
//!   executable path. A common pattern is for the caller to use
//!   `std::env::current_exe()` to relaunch itself in a different mode,
//!   but that is the caller's policy, not this module's.
//!
//! ## Typical Calling Pattern
//!
//! The following sketch shows how a calling application would typically
//! use this module. It is documentation only; the caller owns its own
//! argument handling and error policy.
//!
//! ```ignore
//! use std::env;
//! use launch_split_term_module::{
//!     launch_in_tmux_vertical_split,
//!     LaunchSplitTermError,
//! };
//!
//! // 1. Find the path to the running binary.
//! let current_exe_path = match env::current_exe() {
//!     Ok(path) => path,
//!     Err(io_error) => {
//!         eprintln!("Cannot determine current executable path: {}", io_error);
//!         return;
//!     }
//! };
//!
//! // 2. Convert to &str per the argument encoding policy.
//! let executable_path_str = match current_exe_path.to_str() {
//!     Some(valid_utf8) => valid_utf8,
//!     None => {
//!         eprintln!("Current executable path is not valid UTF-8; cannot launch.");
//!         return;
//!     }
//! };
//!
//! // 3. Build the argument list for the child invocation.
//! let arguments: &[&str] = &[
//!     "--some-flag",
//!     "--another-flag", "value",
//! ];
//!
//! // 4. Hand off to this module.
//! match launch_in_tmux_vertical_split(executable_path_str, arguments) {
//!     Ok(()) => { /* caller continues running in its current pane */ }
//!     Err(launch_error) => {
//!         eprintln!("Tmux vertical split launch failed: {}", launch_error);
//!     }
//! }
//! ```
//!
//! ## Argument Encoding Policy
//!
//! All arguments passed to spawned processes through this module are
//! `&str` (UTF-8). Callers supply arguments as `&[&str]`.
//!
//! ### Scope
//!
//! This module is intended for command-line arguments that are ASCII:
//! flag names (e.g. `--user-name`), short identifiers (e.g. `bob`), and
//! filesystem paths under the caller's control. ASCII is a strict subset
//! of UTF-8, so all ASCII input is valid `&str` input with no conversion
//! and no loss.
//!
//! ### Why `&[&str]`
//!
//! - It is the simplest call site for ASCII flag literals:
//!   `&["--user-name", "bob"]` rather than wrapping each item in
//!   `OsStr::new(...)`.
//! - `&str` converts to `&OsStr` automatically and at zero cost when
//!   passed to `std::process::Command::arg`, which is what this module
//!   does internally.
//! - It forces the caller to validate path and identifier inputs at the
//!   caller's boundary, where the caller has the context to produce a
//!   meaningful error message. This module does not perform lossy
//!   conversions on the caller's behalf.
//!
//! ### Caller Responsibility for Paths
//!
//! When a caller has a `PathBuf` to pass as an argument, the caller
//! must convert it via `path.to_str()`, which returns `Option<&str>`.
//! If the path contains non-UTF-8 bytes (legal but rare on Unix
//! filesystems), the caller must handle the `None` case by reporting
//! an error and not invoking the launcher. This module will not silently
//! corrupt or drop bytes from a non-UTF-8 path.
//!
//! ### Out of Scope
//!
//! - Non-ASCII or non-UTF-8 arguments.
//! - Filesystem paths containing bytes that are not valid UTF-8.
//! - Windows-native UTF-16 path handling.
//!
//! Callers that need any of the above should not use this module.
//!
//! ## Tmux Command-Construction Policy
//!
//! The tmux split launchers build a single space-separated string of the
//! form `"<executable_path> <arg1> <arg2> ..."` and pass that whole string
//! as one argument to `tmux split-window -v` (or `-h`). No shell quoting
//! and no escaping is performed. Callers whose argument values would
//! contain whitespace, shell metacharacters, or quotes must not pass
//! those values through this module; the argument encoding policy above
//! (ASCII flag names, short identifiers, caller-controlled paths) is
//! consistent with that constraint.
//!
//! ## Error Handling Policy
//!
//! Every public function returns `Result<(), LaunchSplitTermError>`.
//! No public function panics, calls `unwrap`, or calls `expect`. The
//! caller is responsible for deciding whether a launch failure is
//! recoverable (e.g. fall back to in-process behavior) or fatal
//! (e.g. exit the program). This module never makes that decision
//! on the caller's behalf.
//!
//! Error messages are deliberately terse and contain no caller data
//! (no paths, no argument values, no environment information) so that
//! a release build cannot leak information through an error path.
//! Each error variant carries a short static tag identifying which
//! function reported the error, to aid log triage:
//!
//!   - `LNT:` — `launch_in_new_terminal`
//!   - `LTV:` — `launch_in_tmux_vertical_split`
//!   - `LTH:` — `launch_in_tmux_horizontal_split`
//!
//! ## Production-Rule Adherence
//!
//! - No `unsafe`.
//! - No `unwrap` / `expect` / `panic!` / `assert!` outside of `#[cfg(test)]`.
//! - No third-party crates; standard library only.
//! - No heap-allocated error payloads carrying caller data; all error
//!   tags are `&'static str`.
//! - `#[cfg(debug_assertions)]` diagnostic prints are stripped from
//!   release builds, so they do not leak data in production binaries.
//! - Bounded use of `format!` and `String` is permitted for building
//!   command strings.

use std::fmt;
use std::process::Command as StdCommand;

// ============================================================================
// Error type
// ============================================================================

/// Errors that can be returned by the public launcher functions in this module.
///
/// Each variant carries only a `&'static str` tag, never any
/// caller-supplied data. This prevents production error paths from
/// leaking paths, environment, or arguments.
///
/// The `&'static str` tag identifies the originating function so that
/// log entries can be triaged without correlating against source line
/// numbers.
#[derive(Debug)]
pub enum LaunchSplitTermError {
    /// A `tmux split-window` invocation failed to spawn. The most common
    /// real-world causes are: tmux is not installed, tmux is not on `PATH`,
    /// or the calling process is not inside a tmux session.
    TmuxSpawnFailed(&'static str),

    /// A new-terminal launch failed because every terminal emulator this
    /// module tried for the current target OS failed to spawn.
    TerminalSpawnFailed(&'static str),

    /// A new-terminal launch was requested on a `target_os` that this
    /// module does not know how to handle. This is a compile-time-known
    /// limitation, but it is returned at runtime so that the caller can
    /// handle it like any other error (e.g. suggest the user use a tmux
    /// split instead).
    UnsupportedPlatform(&'static str),
}

impl fmt::Display for LaunchSplitTermError {
    /// Render a short, production-safe message. The tag prefix identifies
    /// the originating function; no caller data is included.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchSplitTermError::TmuxSpawnFailed(tag) => {
                write!(formatter, "{} tmux split-window failed to spawn", tag)
            }
            LaunchSplitTermError::TerminalSpawnFailed(tag) => {
                write!(formatter, "{} no terminal emulator could be launched", tag)
            }
            LaunchSplitTermError::UnsupportedPlatform(tag) => {
                write!(
                    formatter,
                    "{} new-terminal launch is not implemented for this target_os",
                    tag
                )
            }
        }
    }
}

impl std::error::Error for LaunchSplitTermError {}

// ============================================================================
// Internal helper: build the single-string command tmux expects
// ============================================================================

/// Join an executable path and its arguments into one space-separated
/// command string suitable for passing as the *single* command argument
/// to `tmux split-window -v` (or `-h`).
///
/// # Parameters
///
/// - `executable_path_str`: UTF-8 path to the binary to run inside the
///   new tmux pane.
/// - `arguments`: zero or more UTF-8 arguments to pass to that binary.
///
/// # Returns
///
/// A `String` of the form `"<exe> <arg1> <arg2> ..."`. When `arguments`
/// is empty, returns just `"<exe>"` with no trailing whitespace.
///
/// # Quoting
///
/// Per the module-level tmux command-construction policy, this function
/// performs no shell quoting and no escaping.
///
/// # Visibility
///
/// `pub(crate)` so the test module can exercise it directly without
/// spawning real processes.
pub(crate) fn build_tmux_inner_command_string(
    executable_path_str: &str,
    arguments: &[&str],
) -> String {
    // `arguments.join(" ")` on an empty slice yields "". Using
    // `format!("{} {}", exe, joined)` on an empty argument list would
    // therefore produce `"<exe> "` (one trailing space). Branching on
    // emptiness avoids that trailing space in the no-argument case.
    if arguments.is_empty() {
        executable_path_str.to_string()
    } else {
        format!("{} {}", executable_path_str, arguments.join(" "))
    }
}

// ============================================================================
// Public function: tmux vertical split
// ============================================================================

/// Launch the given executable in a new tmux pane created by a vertical
/// split (`tmux split-window -v`) of the current pane.
///
/// # Parameters
///
/// - `executable_path_str`: UTF-8 path to the binary to run in the new
///   pane. Typically `std::env::current_exe()` converted via `to_str()`.
/// - `arguments`: command-line arguments for that binary, in order.
///   May be empty.
///
/// # Returns
///
/// - `Ok(())` if `tmux split-window -v` was successfully spawned. This
///   means the *tmux command itself* was spawned; it does not guarantee
///   that the inner executable inside the new pane is healthy. tmux is
///   responsible for starting the inner command, and any failure inside
///   that new pane will surface in the new pane, not as an error from
///   this function.
/// - `Err(LaunchSplitTermError::TmuxSpawnFailed(...))` if the `tmux`
///   command could not be spawned. The most common cause is that the
///   parent process is not inside a tmux session, or tmux is not on
///   `PATH`.
///
/// # Prerequisites
///
/// - tmux must be installed and on `PATH`.
/// - The calling process must be running inside a tmux session.
///
/// Both prerequisites are the caller's responsibility to ensure; this
/// function reports a single, terse error if either fails.
///
/// # Side Effects
///
/// On success, the current tmux window gains a new pane below (or
/// beside, depending on the user's tmux configuration) the current
/// pane, running the requested executable.
///
/// # Errors and Production Behavior
///
/// This function never panics. The caller decides how to react to
/// a launch failure.
pub fn launch_in_tmux_vertical_split(
    executable_path_str: &str,
    arguments: &[&str],
) -> Result<(), LaunchSplitTermError> {
    // Build the single space-separated command string that tmux will
    // execute inside the newly-created pane.
    let inner_command_string = build_tmux_inner_command_string(executable_path_str, arguments);

    // Optional debug-only diagnostic. Stripped from release builds, so
    // this cannot leak data in production.
    #[cfg(debug_assertions)]
    eprintln!(
        "LTV: tmux split-window -v with inner command: {}",
        inner_command_string
    );

    // Spawn tmux. We do NOT call `.wait()` or `.output()`; tmux returns
    // promptly after creating the new pane, and the inner process lives
    // inside that pane independent of us.
    let spawn_result = StdCommand::new("tmux")
        .args(["split-window", "-v", &inner_command_string])
        .spawn();

    match spawn_result {
        Ok(_child) => Ok(()),
        Err(_io_error) => {
            // We deliberately discard the inner `io::Error` text: in
            // production we do not echo OS-supplied strings (which may
            // mention paths or environment) into our error path.
            #[cfg(debug_assertions)]
            eprintln!("LTV: tmux spawn failed: {}", _io_error);
            Err(LaunchSplitTermError::TmuxSpawnFailed("LTV:"))
        }
    }
}

// ============================================================================
// Public function: tmux horizontal split
// ============================================================================

/// Launch the given executable in a new tmux pane created by a horizontal
/// split (`tmux split-window -h`) of the current pane.
///
/// This function is identical to `launch_in_tmux_vertical_split` except
/// for the `-h` flag passed to tmux. See that function's documentation
/// for parameter, return, and behavior details.
///
/// # Note on tmux split-flag semantics
///
/// `tmux split-window -h` produces panes arranged side-by-side (a
/// vertical dividing line between them). `tmux split-window -v`
/// produces panes arranged top-to-bottom (a horizontal dividing line
/// between them). The flag-letter convention is tmux's own. The
/// mapping in this module is:
///
///   - `launch_in_tmux_vertical_split`   → `tmux split-window -v`
///   - `launch_in_tmux_horizontal_split` → `tmux split-window -h`
pub fn launch_in_tmux_horizontal_split(
    executable_path_str: &str,
    arguments: &[&str],
) -> Result<(), LaunchSplitTermError> {
    let inner_command_string = build_tmux_inner_command_string(executable_path_str, arguments);

    #[cfg(debug_assertions)]
    eprintln!(
        "LTH: tmux split-window -h with inner command: {}",
        inner_command_string
    );

    let spawn_result = StdCommand::new("tmux")
        .args(["split-window", "-h", &inner_command_string])
        .spawn();

    match spawn_result {
        Ok(_child) => Ok(()),
        Err(_io_error) => {
            #[cfg(debug_assertions)]
            eprintln!("LTH: tmux spawn failed: {}", _io_error);
            Err(LaunchSplitTermError::TmuxSpawnFailed("LTH:"))
        }
    }
}

// ============================================================================
// Public function: new terminal window
// ============================================================================

/// Launch the given executable in a freshly-opened terminal emulator
/// window. The choice of emulator is determined at compile time by
/// `target_os`, with a runtime fallback chain on platforms that have
/// more than one common terminal emulator.
///
/// # Parameters
///
/// - `executable_path_str`: UTF-8 path to the binary to run inside the
///   new terminal window.
/// - `arguments`: command-line arguments for that binary, in order.
///   May be empty.
///
/// # Platform Matrix
///
/// | target_os                                  | Primary           | Fallback |
/// |--------------------------------------------|-------------------|----------|
/// | `linux`, `android`                         | `gnome-terminal`  | `xterm`  |
/// | `macos`                                    | Terminal.app (via `osascript`) | (none) |
/// | `freebsd`, `openbsd`, `netbsd`, `dragonfly`| `xterm`           | (none)   |
/// | `redox`                                    | `terminal`        | (none)   |
/// | anything else                              | returns `UnsupportedPlatform` | — |
///
/// On `macos`, arguments are joined into the AppleScript command using
/// the same single-string space-join policy as the tmux launchers. On
/// every other platform, each argument is passed as a separate process
/// argument to the terminal emulator, so no shell quoting is involved.
///
/// # Returns
///
/// - `Ok(())` if a terminal emulator was successfully spawned.
/// - `Err(LaunchSplitTermError::TerminalSpawnFailed(...))` if every
///   emulator candidate for the current platform failed to spawn.
/// - `Err(LaunchSplitTermError::UnsupportedPlatform(...))` on a
///   `target_os` not covered by the matrix above.
///
/// # Errors and Production Behavior
///
/// This function never panics. If it returns an error, the caller may
/// wish to advise the user to fall back to a tmux split, or to install
/// a terminal emulator, depending on the application's policy.
pub fn launch_in_new_terminal(
    executable_path_str: &str,
    arguments: &[&str],
) -> Result<(), LaunchSplitTermError> {
    #[cfg(debug_assertions)]
    eprintln!(
        "LNT: requested new-terminal launch of {} with {} arg(s)",
        executable_path_str,
        arguments.len()
    );

    // ─────────────────────────────────────────────────────────────
    // Linux and Android (Termux): try gnome-terminal, then xterm.
    // ─────────────────────────────────────────────────────────────
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Attempt 1: gnome-terminal.
        // The `--` separator tells gnome-terminal that everything after
        // it is the command + its arguments, not more gnome-terminal flags.
        let gnome_attempt = StdCommand::new("gnome-terminal")
            .arg("--")
            .arg(executable_path_str)
            .args(arguments)
            .spawn();

        if gnome_attempt.is_ok() {
            #[cfg(debug_assertions)]
            eprintln!("LNT: launched via gnome-terminal");
            return Ok(());
        }

        // Attempt 2: xterm.
        // xterm's `-e` flag means "execute the rest as the command".
        let xterm_attempt = StdCommand::new("xterm")
            .arg("-e")
            .arg(executable_path_str)
            .args(arguments)
            .spawn();

        if xterm_attempt.is_ok() {
            #[cfg(debug_assertions)]
            eprintln!("LNT: launched via xterm (fallback)");
            return Ok(());
        }

        #[cfg(debug_assertions)]
        eprintln!("LNT: both gnome-terminal and xterm failed");
        return Err(LaunchSplitTermError::TerminalSpawnFailed("LNT:"));
    }

    // ─────────────────────────────────────────────────────────────
    // macOS: drive Terminal.app via osascript.
    // ─────────────────────────────────────────────────────────────
    #[cfg(target_os = "macos")]
    {
        // AppleScript's `do script` takes a single shell command line,
        // so we use the same space-join helper as the tmux launchers
        // to assemble the executable path and its arguments.
        let inner_command_string = build_tmux_inner_command_string(executable_path_str, arguments);

        // Construct the AppleScript snippet. This embeds the command
        // string inside a double-quoted AppleScript literal. Per the
        // module-level argument encoding policy, callers do not pass
        // values containing double quotes or shell metacharacters, so
        // no escaping layer is added here.
        let apple_script_source = format!(
            "tell application \"Terminal\" to do script \"{}\"",
            inner_command_string
        );

        let osascript_attempt = StdCommand::new("osascript")
            .arg("-e")
            .arg(&apple_script_source)
            .spawn();

        match osascript_attempt {
            Ok(_child) => {
                #[cfg(debug_assertions)]
                eprintln!("LNT: launched via Terminal.app (osascript)");
                return Ok(());
            }
            Err(_io_error) => {
                #[cfg(debug_assertions)]
                eprintln!("LNT: osascript spawn failed: {}", _io_error);
                return Err(LaunchSplitTermError::TerminalSpawnFailed("LNT:"));
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // BSD family: xterm.
    // ─────────────────────────────────────────────────────────────
    #[cfg(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
    ))]
    {
        let xterm_attempt = StdCommand::new("xterm")
            .arg("-e")
            .arg(executable_path_str)
            .args(arguments)
            .spawn();

        match xterm_attempt {
            Ok(_child) => {
                #[cfg(debug_assertions)]
                eprintln!("LNT: launched via xterm (BSD)");
                return Ok(());
            }
            Err(_io_error) => {
                #[cfg(debug_assertions)]
                eprintln!("LNT: xterm spawn failed: {}", _io_error);
                return Err(LaunchSplitTermError::TerminalSpawnFailed("LNT:"));
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // Redox: `terminal`.
    // ─────────────────────────────────────────────────────────────
    #[cfg(target_os = "redox")]
    {
        let redox_attempt = StdCommand::new("terminal")
            .arg(executable_path_str)
            .args(arguments)
            .spawn();

        match redox_attempt {
            Ok(_child) => {
                #[cfg(debug_assertions)]
                eprintln!("LNT: launched via Redox terminal");
                return Ok(());
            }
            Err(_io_error) => {
                #[cfg(debug_assertions)]
                eprintln!("LNT: Redox terminal spawn failed: {}", _io_error);
                return Err(LaunchSplitTermError::TerminalSpawnFailed("LNT:"));
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // Catch-all: target_os not covered above.
    //
    // This block is only compiled in when none of the above cfg blocks
    // apply. Without it, a target like `windows` or `solaris` would
    // produce a function with no return path, which would not compile.
    // ─────────────────────────────────────────────────────────────
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "redox",
    )))]
    {
        #[cfg(debug_assertions)]
        eprintln!("LNT: target_os is not in the supported matrix");
        Err(LaunchSplitTermError::UnsupportedPlatform("LNT:"))
    }
}

// ============================================================================
// Tests
//
// These tests intentionally do NOT spawn any real processes. Spawning
// `tmux`, `gnome-terminal`, or `osascript` from a unit test would be
// non-hermetic (depends on the host environment), non-portable across
// CI runners, and slow. The only behavior we test here is the pure
// string assembly used to build the tmux inner command, plus the
// `Display` output of the error type, both of which are deterministic
// and safe to assert on.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_tmux_inner_command_string` with no arguments returns just
    /// the executable path with no trailing whitespace.
    #[test]
    fn build_tmux_inner_command_string_no_arguments() {
        let result = build_tmux_inner_command_string("/usr/bin/some_binary", &[]);
        assert_eq!(result, "/usr/bin/some_binary");
    }

    /// `build_tmux_inner_command_string` with one argument joins with a
    /// single space.
    #[test]
    fn build_tmux_inner_command_string_one_argument() {
        let result = build_tmux_inner_command_string("/usr/bin/some_binary", &["--flag-name"]);
        assert_eq!(result, "/usr/bin/some_binary --flag-name");
    }

    /// `build_tmux_inner_command_string` with several arguments joins
    /// all of them with single spaces in order.
    #[test]
    fn build_tmux_inner_command_string_many_arguments() {
        let result = build_tmux_inner_command_string(
            "/opt/app/the_binary",
            &["--user-name", "bob", "--log-path", "/tmp/log"],
        );
        assert_eq!(
            result,
            "/opt/app/the_binary --user-name bob --log-path /tmp/log"
        );
    }

    /// The `Display` impl for `TmuxSpawnFailed` includes the function
    /// tag and a short fixed message, and does not include caller data.
    #[test]
    fn display_tmux_spawn_failed_includes_tag() {
        let error = LaunchSplitTermError::TmuxSpawnFailed("LTV:");
        let rendered = format!("{}", error);
        assert!(rendered.starts_with("LTV:"));
        assert!(rendered.contains("tmux"));
    }

    /// The `Display` impl for `TerminalSpawnFailed` includes the function
    /// tag and a short fixed message.
    #[test]
    fn display_terminal_spawn_failed_includes_tag() {
        let error = LaunchSplitTermError::TerminalSpawnFailed("LNT:");
        let rendered = format!("{}", error);
        assert!(rendered.starts_with("LNT:"));
        assert!(rendered.contains("terminal"));
    }

    /// The `Display` impl for `UnsupportedPlatform` includes the function
    /// tag and mentions that the platform is unsupported.
    #[test]
    fn display_unsupported_platform_includes_tag() {
        let error = LaunchSplitTermError::UnsupportedPlatform("LNT:");
        let rendered = format!("{}", error);
        assert!(rendered.starts_with("LNT:"));
        assert!(rendered.contains("target_os"));
    }
}
