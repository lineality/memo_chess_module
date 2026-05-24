//! # memo_chess_tui — Binary Entry Point
//!
//! ## Project Context
//!
//! `memo_chess_tui` is a terminal-displayed chess game where player moves
//! arrive as TOML memo files in a flat directory. This binary is the
//! user-facing entry point. It is responsible for:
//!
//!   1. Parsing command-line arguments.
//!   2. Dispatching to one of:
//!         - print help,
//!         - print version,
//!         - run the demo inline,
//!         - run the real game inline,
//!         - re-launch itself in a new terminal window,
//!         - re-launch itself in a new tmux vertical split,
//!         - re-launch itself in a new tmux horizontal split.
//!
//! ## Why a Thin Dispatcher
//!
//! The actual chess engine and game loop live in `memo_chess_tui_module`.
//! Window/terminal placement lives in `launch_split_term_module`. This
//! file owns only:
//!
//!   - command-line parsing,
//!   - argument-forwarding when re-launching,
//!   - help/version/exit-code policy.
//!
//! Keeping this binary thin means a layout change in tmux behavior, or a
//! new CLI flag, only touches one of the small modules — never the chess
//! rules engine.
//!
//! ## Mode Selection
//!
//! Exactly one launch-mode flag may be supplied per invocation. If two
//! launch-mode flags are present (e.g. `-nt -tv`) parsing fails with a
//! usage error and the program exits non-zero.
//!
//! `--demo-test` is **not** a launch-mode flag — it is an input selector
//! that says "use hard-coded demo inputs instead of the four `--*-path`
//! and `--user-name` flags". `--demo-test` therefore composes with any
//! launch-mode flag.
//!
//! Default behavior with no flags is to print help and exit 0.
//!
//! ## Production-Rule Adherence
//!
//! - No `unsafe`.
//! - No `unwrap` / `expect` / `panic!` in real code paths. All such uses
//!   are confined to `#[cfg(test)]` blocks.
//! - No third-party crates. CLI parsing is hand-rolled over the standard
//!   library only.
//! - All error paths return an `ExitCode` via `std::process::ExitCode`.

// ───────────────────────────────────────────────────────────────────────────
// Module declarations.
//
// `launch_split_term_module` provides the three terminal/pane launchers.
// `memo_chess_tui_module` provides the chess engine and game loop.
// ───────────────────────────────────────────────────────────────────────────

mod launch_split_term_module;
mod memo_chess_tui_module;

use launch_split_term_module::{
    LaunchSplitTermError, launch_in_new_terminal, launch_in_tmux_horizontal_split,
    launch_in_tmux_vertical_split,
};

use std::path::Path;
use std::process::ExitCode;

// ───────────────────────────────────────────────────────────────────────────
// Exit-code constants.
//
// Process exit codes are part of this binary's public surface (scripts,
// CI, supervisors will read them). They are therefore named and stable.
// ───────────────────────────────────────────────────────────────────────────

/// Successful completion or "informational" exit (help, version).
const EXIT_CODE_SUCCESS: u8 = 0;

/// The bootstrap or game-loop layer reported an unrecoverable error.
const EXIT_CODE_RUNTIME_FAILURE: u8 = 1;

/// The user supplied an invalid combination of command-line arguments.
const EXIT_CODE_USAGE_ERROR: u8 = 2;

/// A re-launch into a new terminal window or tmux pane failed.
const EXIT_CODE_LAUNCH_FAILURE: u8 = 3;

/// The current executable path could not be determined or was not valid
/// UTF-8. This blocks any re-launch.
const EXIT_CODE_CURRENT_EXE_UNAVAILABLE: u8 = 4;

// ───────────────────────────────────────────────────────────────────────────
// Parsed-CLI types.
// ───────────────────────────────────────────────────────────────────────────

/// The top-level action the binary will take after argument parsing.
///
/// Exactly one of these is selected per invocation. The default, chosen
/// when the user passes no flags at all, is `PrintHelp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchModeSelection {
    /// Print the help text and exit 0.
    PrintHelp,
    /// Print the version string and exit 0.
    PrintVersion,
    /// Run the chess game inline in the current terminal.
    RunInlineInCurrentTerminal,
    /// Re-launch the current executable in a fresh terminal-emulator window.
    RelaunchInNewTerminalWindow,
    /// Re-launch the current executable in a new tmux pane (vertical split).
    RelaunchInTmuxVerticalSplit,
    /// Re-launch the current executable in a new tmux pane (horizontal split).
    RelaunchInTmuxHorizontalSplit,
}

/// The fully-parsed command-line state.
///
/// All input-value fields are `Option<String>`. They are only required
/// to be `Some(_)` when `is_demo_mode_requested` is `false` *and* the
/// resolved launch mode actually needs to read them (i.e. running the
/// game inline). Validation of "all four present" happens at run time,
/// not at parse time, so parse errors and validation errors stay
/// distinct in the error path.
#[derive(Debug, Clone)]
struct ParsedCommandLineArguments {
    /// Which top-level action to perform.
    launch_mode_selection: LaunchModeSelection,

    /// If `true`, run with the hard-coded demo inputs instead of the
    /// four `--*-path` / `--user-name` flags. Composes with any launch
    /// mode: re-launch will forward `--demo-test` to the child.
    is_demo_mode_requested: bool,

    /// Directory containing player memo files (TOML).
    memo_files_directory_path: Option<String>,

    /// Local user name used for memo attribution.
    local_user_name: Option<String>,

    /// Directory where logs are written.
    logging_directory_path: Option<String>,

    /// Working directory for the chrono-index module.
    chrono_sort_temp_directory_path: Option<String>,
}

// ───────────────────────────────────────────────────────────────────────────
// `main` — thin dispatcher.
// ───────────────────────────────────────────────────────────────────────────

/// Program entry point.
///
/// Returns `ExitCode` so that *every* exit path is explicit and visible.
/// No `process::exit` calls are needed and no panics are possible from
/// this function in production builds.
fn main() -> ExitCode {
    // Collect raw argv. We keep ownership of the original `Vec<String>`
    // because the re-launch paths need to forward a (filtered) copy of
    // it to a child process.
    let raw_argument_vector: Vec<String> = std::env::args().collect();

    // ── 1. Parse ────────────────────────────────────────────────────────
    let parsed_arguments = match parse_command_line_arguments(&raw_argument_vector) {
        Ok(parsed_value) => parsed_value,
        Err(parse_error_message) => {
            eprintln!("Error: {}", parse_error_message);
            eprintln!();
            print_help_text_to_stdout();
            return ExitCode::from(EXIT_CODE_USAGE_ERROR);
        }
    };

    // ── 2. Dispatch ─────────────────────────────────────────────────────
    match parsed_arguments.launch_mode_selection {
        LaunchModeSelection::PrintHelp => {
            print_help_text_to_stdout();
            ExitCode::from(EXIT_CODE_SUCCESS)
        }

        LaunchModeSelection::PrintVersion => {
            print_version_text_to_stdout();
            ExitCode::from(EXIT_CODE_SUCCESS)
        }

        LaunchModeSelection::RunInlineInCurrentTerminal => {
            run_inline_in_current_terminal(&parsed_arguments)
        }

        LaunchModeSelection::RelaunchInNewTerminalWindow => relaunch_self_via(
            &raw_argument_vector,
            &parsed_arguments,
            RelaunchKind::NewTerminalWindow,
        ),

        LaunchModeSelection::RelaunchInTmuxVerticalSplit => relaunch_self_via(
            &raw_argument_vector,
            &parsed_arguments,
            RelaunchKind::TmuxVerticalSplit,
        ),

        LaunchModeSelection::RelaunchInTmuxHorizontalSplit => relaunch_self_via(
            &raw_argument_vector,
            &parsed_arguments,
            RelaunchKind::TmuxHorizontalSplit,
        ),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// CLI parsing.
// ───────────────────────────────────────────────────────────────────────────

/// Parse the argv vector into a `ParsedCommandLineArguments`.
///
/// This is a hand-rolled, single-pass parser. It deliberately does not
/// validate "all four inputs present" — that check happens only when the
/// resolved launch mode actually needs the inputs, so the user sees a
/// targeted error rather than a generic one.
///
/// # Parsing rules
///
/// - The first element of `raw_argument_vector` is the program name and
///   is skipped.
/// - Unknown tokens produce an error.
/// - Two launch-mode flags in the same invocation produce an error.
/// - A value-taking flag with no following value produces an error.
///
/// # Returns
///
/// - `Ok(ParsedCommandLineArguments)` on a syntactically valid command
///   line.
/// - `Err(String)` carrying a human-readable usage error otherwise. The
///   caller is expected to print this and the help text and exit
///   non-zero.
fn parse_command_line_arguments(
    raw_argument_vector: &[String],
) -> Result<ParsedCommandLineArguments, String> {
    // Track the selected launch-mode flag (if any) so we can detect the
    // "two launch modes given" error.
    let mut selected_launch_mode: Option<LaunchModeSelection> = None;

    // Composable input flags.
    let mut is_demo_mode_requested: bool = false;
    let mut memo_files_directory_path: Option<String> = None;
    let mut local_user_name: Option<String> = None;
    let mut logging_directory_path: Option<String> = None;
    let mut chrono_sort_temp_directory_path: Option<String> = None;

    // Iterate over argv, skipping argv[0] (the program name).
    let mut argv_iterator = raw_argument_vector.iter().skip(1);

    while let Some(current_argument) = argv_iterator.next() {
        match current_argument.as_str() {
            // ─── Launch-mode flags ──────────────────────────────────────
            "-h" | "--help" => {
                assign_launch_mode_or_conflict(
                    &mut selected_launch_mode,
                    LaunchModeSelection::PrintHelp,
                )?;
            }
            "-v" | "--version" => {
                assign_launch_mode_or_conflict(
                    &mut selected_launch_mode,
                    LaunchModeSelection::PrintVersion,
                )?;
            }
            "-nt" | "--new-terminal" => {
                assign_launch_mode_or_conflict(
                    &mut selected_launch_mode,
                    LaunchModeSelection::RelaunchInNewTerminalWindow,
                )?;
            }
            "-tv" | "--tmux-split-vertical" => {
                assign_launch_mode_or_conflict(
                    &mut selected_launch_mode,
                    LaunchModeSelection::RelaunchInTmuxVerticalSplit,
                )?;
            }
            "-th" | "--tmux-split-horizontal" => {
                assign_launch_mode_or_conflict(
                    &mut selected_launch_mode,
                    LaunchModeSelection::RelaunchInTmuxHorizontalSplit,
                )?;
            }

            // ─── Input selector (composes with launch-mode flags) ───────
            "--demo-test" => {
                is_demo_mode_requested = true;
            }

            // ─── Value-taking input flags ───────────────────────────────
            "--memo-file-dir-path" => {
                memo_files_directory_path = Some(consume_required_value(
                    &mut argv_iterator,
                    "--memo-file-dir-path",
                )?);
            }
            "--user-name" => {
                local_user_name = Some(consume_required_value(&mut argv_iterator, "--user-name")?);
            }
            "--log-path" => {
                logging_directory_path =
                    Some(consume_required_value(&mut argv_iterator, "--log-path")?);
            }
            "--chronosort-path" => {
                chrono_sort_temp_directory_path = Some(consume_required_value(
                    &mut argv_iterator,
                    "--chronosort-path",
                )?);
            }

            // ─── Anything else is an error ──────────────────────────────
            unknown_token => {
                return Err(format!("Unknown argument: {}", unknown_token));
            }
        }
    }

    // Resolve the launch mode:
    //
    //   - If the user gave a launch-mode flag, use it.
    //   - Otherwise, if the user gave any input flag (--demo-test or any
    //     of the four value flags), they meant to run inline.
    //   - Otherwise, the user passed nothing: print help.
    let resolved_launch_mode = match selected_launch_mode {
        Some(explicit_mode) => explicit_mode,
        None => {
            let any_input_supplied = is_demo_mode_requested
                || memo_files_directory_path.is_some()
                || local_user_name.is_some()
                || logging_directory_path.is_some()
                || chrono_sort_temp_directory_path.is_some();

            if any_input_supplied {
                LaunchModeSelection::RunInlineInCurrentTerminal
            } else {
                LaunchModeSelection::PrintHelp
            }
        }
    };

    Ok(ParsedCommandLineArguments {
        launch_mode_selection: resolved_launch_mode,
        is_demo_mode_requested,
        memo_files_directory_path,
        local_user_name,
        logging_directory_path,
        chrono_sort_temp_directory_path,
    })
}

/// Assign the launch-mode slot, reporting an error if it is already set.
///
/// This enforces the rule that exactly one launch-mode flag may be given
/// per invocation.
fn assign_launch_mode_or_conflict(
    launch_mode_slot: &mut Option<LaunchModeSelection>,
    candidate_mode: LaunchModeSelection,
) -> Result<(), String> {
    if launch_mode_slot.is_some() {
        return Err("Multiple launch-mode flags supplied; pass only one of \
             -h/--help, -v/--version, -nt/--new-terminal, \
             -tv/--tmux-split-vertical, -th/--tmux-split-horizontal."
            .to_string());
    }
    *launch_mode_slot = Some(candidate_mode);
    Ok(())
}

/// Consume the next argv token as the value for a value-taking flag.
///
/// Returns an error if there is no next token, naming the offending flag
/// in the error message.
fn consume_required_value<'iterator_lifetime>(
    argv_iterator: &mut impl Iterator<Item = &'iterator_lifetime String>,
    flag_name_for_error: &str,
) -> Result<String, String> {
    match argv_iterator.next() {
        Some(value_token) => Ok(value_token.clone()),
        None => Err(format!(
            "Flag '{}' requires a value but none was given.",
            flag_name_for_error
        )),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Help and version output.
// ───────────────────────────────────────────────────────────────────────────

/// Print the canonical help text.
///
/// Help is informational, so it goes to stdout. Errors that print help
/// as a hint also call this function, but they print their error first
/// to stderr.
fn print_help_text_to_stdout() {
    let help_text = "\
memo_chess_tui — terminal chess driven by TOML memo files

USAGE:
    memo_chess_tui [LAUNCH-MODE] [INPUT-FLAGS...]

LAUNCH MODES (pick at most one):
    -h,  --help                       Print this help and exit
    -v,  --version                    Print version and exit
    -nt, --new-terminal               Re-launch in a new terminal window
    -tv, --tmux-split-vertical        Re-launch in a new tmux pane (vertical split)
    -th, --tmux-split-horizontal      Re-launch in a new tmux pane (horizontal split)

INPUT FLAGS:
         --demo-test                  Use hard-coded demo inputs (no other input
                                      flags are required). Composes with -nt / -tv / -th.

    The following four flags are required for a non-demo run:
         --memo-file-dir-path  <PATH> Directory containing player memo files
         --user-name           <NAME> Local user name for memo attribution
         --log-path            <PATH> Directory where logs are written
         --chronosort-path     <PATH> Working directory for the chrono-index module

BEHAVIOR:
    - With no flags at all, this help is printed.
    - With --demo-test (alone or with a launch mode), the demo runs.
    - For a non-demo run, all four input flags above are required.
    - Re-launch modes spawn a child process that re-parses the same flags
      (minus the launch-mode flag), so a non-demo re-launch must include
      the four input flags.

EXAMPLES:
    memo_chess_tui --demo-test
    memo_chess_tui --demo-test -tv
    memo_chess_tui --memo-file-dir-path ./game --user-name bob \\
                   --log-path ./logs --chronosort-path ./chrono
";
    // We deliberately use `println!` here because help output is
    // informational and not on any hot path. A broken-pipe panic on
    // stdout from `println!` during `--help` is acceptable behavior
    // (the user piped to `head` etc.).
    print!("{}", help_text);
}

/// Print the version string sourced from Cargo at compile time.
///
/// `CARGO_PKG_VERSION` is provided by Cargo for every crate it builds,
/// so this is infallible.
fn print_version_text_to_stdout() {
    println!("memo_chess_tui {}", env!("CARGO_PKG_VERSION"));
}

// ───────────────────────────────────────────────────────────────────────────
// Inline run paths (the actual game).
// ───────────────────────────────────────────────────────────────────────────

/// Resolve which set of inputs to run with and dispatch to the engine.
///
/// - If `--demo-test` was given, the four hard-coded demo paths are used.
/// - Otherwise, all four `--*-path` / `--user-name` flags must be present;
///   missing any of them is a usage error.
fn run_inline_in_current_terminal(parsed_arguments: &ParsedCommandLineArguments) -> ExitCode {
    if parsed_arguments.is_demo_mode_requested {
        run_hardcoded_demo()
    } else {
        match resolve_real_run_inputs(parsed_arguments) {
            Ok(resolved_inputs) => run_real_game_with_inputs(resolved_inputs),
            Err(usage_error_message) => {
                eprintln!("Error: {}", usage_error_message);
                eprintln!();
                print_help_text_to_stdout();
                ExitCode::from(EXIT_CODE_USAGE_ERROR)
            }
        }
    }
}

/// The four owned strings needed to start a non-demo game.
struct ResolvedRealRunInputs {
    memo_files_directory_path: String,
    local_user_name: String,
    logging_directory_path: String,
    chrono_sort_temp_directory_path: String,
}

/// Confirm that all four required input flags are present for a
/// non-demo inline run, returning them in a single struct.
fn resolve_real_run_inputs(
    parsed_arguments: &ParsedCommandLineArguments,
) -> Result<ResolvedRealRunInputs, String> {
    let memo_files_directory_path = match &parsed_arguments.memo_files_directory_path {
        Some(value_str) => value_str.clone(),
        None => return Err("Missing required flag: --memo-file-dir-path".to_string()),
    };
    let local_user_name = match &parsed_arguments.local_user_name {
        Some(value_str) => value_str.clone(),
        None => return Err("Missing required flag: --user-name".to_string()),
    };
    let logging_directory_path = match &parsed_arguments.logging_directory_path {
        Some(value_str) => value_str.clone(),
        None => return Err("Missing required flag: --log-path".to_string()),
    };
    let chrono_sort_temp_directory_path = match &parsed_arguments.chrono_sort_temp_directory_path {
        Some(value_str) => value_str.clone(),
        None => return Err("Missing required flag: --chronosort-path".to_string()),
    };

    Ok(ResolvedRealRunInputs {
        memo_files_directory_path,
        local_user_name,
        logging_directory_path,
        chrono_sort_temp_directory_path,
    })
}

/// Run the game with caller-supplied (CLI-derived) inputs.
fn run_real_game_with_inputs(resolved_inputs: ResolvedRealRunInputs) -> ExitCode {
    drive_full_game_pipeline(
        Path::new(&resolved_inputs.memo_files_directory_path),
        resolved_inputs.local_user_name.as_bytes(),
        Path::new(&resolved_inputs.logging_directory_path),
        Path::new(&resolved_inputs.chrono_sort_temp_directory_path),
    )
}

/// Run the game with the hard-coded demo inputs.
///
/// These match the paths used in the original MVP-1 demo and exist so a
/// developer can verify the engine end-to-end with one command. Real
/// (non-demo) deployments must supply the four input flags explicitly.
fn run_hardcoded_demo() -> ExitCode {
    // Demo inputs — intentionally hard-coded to preserve the original
    // MVP-1 demo behavior referenced in this binary's history.
    let demo_memo_files_directory = Path::new("./test_game_files_5_draw");
    let demo_chrono_sort_temp_directory = Path::new("./test_chrono_temp");
    let demo_logging_directory = Path::new("./test_logs");
    let demo_local_user_name: &[u8] = b"bob";

    drive_full_game_pipeline(
        demo_memo_files_directory,
        demo_local_user_name,
        demo_logging_directory,
        demo_chrono_sort_temp_directory,
    )
}

/// Execute the full bootstrap → state-init → replay → game-loop pipeline.
///
/// This is the single point where this binary talks to the chess engine.
/// Both the demo path and the real path funnel through here so the
/// pipeline is described exactly once.
///
/// # Returns
///
/// - `EXIT_CODE_SUCCESS` on a clean game completion.
/// - `EXIT_CODE_RUNTIME_FAILURE` if bootstrap fails.
fn drive_full_game_pipeline(
    game_files_directory_path: &Path,
    local_user_name_bytes: &[u8],
    logging_directory_path: &Path,
    chrono_sort_temp_directory_path: &Path,
) -> ExitCode {
    // Banner — purely informational.
    println!("═══════════════════════════════════════════════════════════");
    println!("Memo-Chess");
    println!("═══════════════════════════════════════════════════════════");
    println!();
    println!(
        "Game files directory:  {}",
        game_files_directory_path.display()
    );
    println!(
        "Chrono-index temp dir: {}",
        chrono_sort_temp_directory_path.display()
    );
    println!(
        "Logging directory:     {}",
        logging_directory_path.display()
    );
    println!(
        "Local user name:       {}",
        String::from_utf8_lossy(local_user_name_bytes)
    );
    println!();
    println!("Waiting for bootstrap configuration...");
    println!();

    // Step 1: bootstrap configuration from memos.
    let bootstrap_result = memo_chess_tui_module::q_and_a_setup_bootstrap(
        game_files_directory_path,
        local_user_name_bytes,
        chrono_sort_temp_directory_path,
        logging_directory_path,
    );

    let game_configuration = match bootstrap_result {
        Ok(configuration_value) => configuration_value,
        Err(bootstrap_error_value) => {
            eprintln!();
            eprintln!("Bootstrap failed: {:?}", bootstrap_error_value);
            eprintln!("Unable to start game. Check configuration and try again.");
            return ExitCode::from(EXIT_CODE_RUNTIME_FAILURE);
        }
    };

    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!("Configuration complete. Starting game...");
    println!("═══════════════════════════════════════════════════════════");
    println!();

    // Step 2: build the initial dungeon-master state.
    let initial_dungeon_master_state =
        memo_chess_tui_module::create_initial_dungeon_master_state(game_configuration);

    // Step 3: replay any existing moves from the chrono index.
    let post_replay_dungeon_master_state =
        memo_chess_tui_module::replay_existing_moves_from_chrono_index(
            initial_dungeon_master_state,
        );

    // Step 4: run the main game loop until termination.
    let final_dungeon_master_state =
        memo_chess_tui_module::run_memochess_dungeon_master_loop(post_replay_dungeon_master_state);

    // Step 5: summary.
    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!("Game Over");
    println!("═══════════════════════════════════════════════════════════");
    println!();
    println!(
        "Final game status: {:?}",
        final_dungeon_master_state.board.game_status
    );
    println!(
        "Final move number: {}",
        final_dungeon_master_state.board.fullmove_number
    );
    println!(
        "Game files written to:  {}",
        game_files_directory_path.display()
    );
    println!(
        "Logs written to:        {}",
        logging_directory_path.display()
    );
    println!();

    ExitCode::from(EXIT_CODE_SUCCESS)
}

// ───────────────────────────────────────────────────────────────────────────
// Re-launch paths (delegating to `launch_split_term_module`).
// ───────────────────────────────────────────────────────────────────────────

/// Which kind of re-launch is being performed. Internal to `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelaunchKind {
    NewTerminalWindow,
    TmuxVerticalSplit,
    TmuxHorizontalSplit,
}

/// Re-launch the current executable in the requested terminal/pane.
///
/// The child invocation inherits all original argv tokens **except** the
/// launch-mode flag that triggered this re-launch. As a result:
///
///   - `memo_chess_tui --demo-test -nt`      → child sees `--demo-test`
///   - `memo_chess_tui -tv --user-name bob …` → child sees `--user-name bob …`
///
/// The child will then re-parse, see no launch-mode flag, and dispatch
/// to `RunInlineInCurrentTerminal`.
///
/// # Validation
///
/// For a non-demo re-launch we *also* require the four input flags up
/// front, so the parent can give a clear error rather than the child
/// failing inside the new pane where the error is harder to read.
fn relaunch_self_via(
    raw_argument_vector: &[String],
    parsed_arguments: &ParsedCommandLineArguments,
    relaunch_kind: RelaunchKind,
) -> ExitCode {
    // Pre-flight: a non-demo re-launch must carry the four input flags.
    if !parsed_arguments.is_demo_mode_requested {
        if let Err(usage_error_message) = resolve_real_run_inputs(parsed_arguments) {
            eprintln!("Error: {}", usage_error_message);
            eprintln!("(Required when re-launching without --demo-test.)");
            eprintln!();
            print_help_text_to_stdout();
            return ExitCode::from(EXIT_CODE_USAGE_ERROR);
        }
    }

    // Identify the current executable path (UTF-8).
    let current_executable_path_buf = match std::env::current_exe() {
        Ok(path_buf_value) => path_buf_value,
        Err(io_error_value) => {
            eprintln!(
                "Error: cannot determine current executable path: {}",
                io_error_value
            );
            return ExitCode::from(EXIT_CODE_CURRENT_EXE_UNAVAILABLE);
        }
    };
    let current_executable_path_str = match current_executable_path_buf.to_str() {
        Some(utf8_str) => utf8_str.to_string(),
        None => {
            eprintln!("Error: current executable path is not valid UTF-8; cannot re-launch.");
            return ExitCode::from(EXIT_CODE_CURRENT_EXE_UNAVAILABLE);
        }
    };

    // Build the child's argv (excluding the launch-mode flag).
    let child_argument_strings =
        build_child_argument_vector_for_relaunch(raw_argument_vector, relaunch_kind);

    // `launch_split_term_module` takes `&[&str]`, so produce that view.
    let child_argument_borrowed_slice: Vec<&str> = child_argument_strings
        .iter()
        .map(|owned_string| owned_string.as_str())
        .collect();

    // Dispatch to the requested launcher.
    let launch_result: Result<(), LaunchSplitTermError> = match relaunch_kind {
        RelaunchKind::NewTerminalWindow => {
            launch_in_new_terminal(&current_executable_path_str, &child_argument_borrowed_slice)
        }
        RelaunchKind::TmuxVerticalSplit => launch_in_tmux_vertical_split(
            &current_executable_path_str,
            &child_argument_borrowed_slice,
        ),
        RelaunchKind::TmuxHorizontalSplit => launch_in_tmux_horizontal_split(
            &current_executable_path_str,
            &child_argument_borrowed_slice,
        ),
    };

    match launch_result {
        Ok(()) => {
            // Parent's job is done; the child runs independently in its
            // new terminal or pane. Exit cleanly.
            ExitCode::from(EXIT_CODE_SUCCESS)
        }
        Err(launch_error_value) => {
            eprintln!("Launch failed: {}", launch_error_value);
            ExitCode::from(EXIT_CODE_LAUNCH_FAILURE)
        }
    }
}

/// Build the argv that should be passed to the child re-launch.
///
/// The returned vector contains every token of `raw_argument_vector`
/// **except** argv[0] (the program name) and any token that matches a
/// short or long form of the launch-mode flag being stripped.
///
/// Pure function — testable without spawning anything.
fn build_child_argument_vector_for_relaunch(
    raw_argument_vector: &[String],
    relaunch_kind: RelaunchKind,
) -> Vec<String> {
    // Tokens to strip, by launch kind. Both short and long forms are
    // removed in case the user passed either.
    let flag_tokens_to_strip: &[&str] = match relaunch_kind {
        RelaunchKind::NewTerminalWindow => &["-nt", "--new-terminal"],
        RelaunchKind::TmuxVerticalSplit => &["-tv", "--tmux-split-vertical"],
        RelaunchKind::TmuxHorizontalSplit => &["-th", "--tmux-split-horizontal"],
    };

    raw_argument_vector
        .iter()
        .skip(1) // drop argv[0] (program name)
        .filter(|candidate_token| !flag_tokens_to_strip.contains(&candidate_token.as_str()))
        .cloned()
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// Tests.
//
// Tests cover the pure-logic surface: CLI parsing, conflict detection,
// child-argv construction. Spawning a real terminal or tmux is out of
// scope (non-hermetic, environment-dependent) — that is the launcher
// module's concern, and it already declines to spawn from its own tests.
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: turn a list of &str into the `Vec<String>` shape
    /// that `parse_command_line_arguments` expects (with a synthetic
    /// program name at index 0).
    fn make_fake_argv(tokens_after_program_name: &[&str]) -> Vec<String> {
        let mut argv_vector = vec!["memo_chess_tui".to_string()];
        for token in tokens_after_program_name {
            argv_vector.push((*token).to_string());
        }
        argv_vector
    }

    #[test]
    fn no_flags_resolves_to_print_help() {
        let argv_vector = make_fake_argv(&[]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        assert_eq!(parsed.launch_mode_selection, LaunchModeSelection::PrintHelp);
        assert!(!parsed.is_demo_mode_requested);
    }

    #[test]
    fn dash_h_resolves_to_print_help() {
        let argv_vector = make_fake_argv(&["-h"]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        assert_eq!(parsed.launch_mode_selection, LaunchModeSelection::PrintHelp);
    }

    #[test]
    fn dash_v_resolves_to_print_version() {
        let argv_vector = make_fake_argv(&["--version"]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        assert_eq!(
            parsed.launch_mode_selection,
            LaunchModeSelection::PrintVersion
        );
    }

    #[test]
    fn demo_test_alone_resolves_to_inline_with_demo_flag() {
        let argv_vector = make_fake_argv(&["--demo-test"]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        assert_eq!(
            parsed.launch_mode_selection,
            LaunchModeSelection::RunInlineInCurrentTerminal
        );
        assert!(parsed.is_demo_mode_requested);
    }

    #[test]
    fn demo_test_with_tmux_vertical_composes() {
        let argv_vector = make_fake_argv(&["--demo-test", "-tv"]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        assert_eq!(
            parsed.launch_mode_selection,
            LaunchModeSelection::RelaunchInTmuxVerticalSplit
        );
        assert!(parsed.is_demo_mode_requested);
    }

    #[test]
    fn four_inputs_without_launch_mode_resolves_to_inline() {
        let argv_vector = make_fake_argv(&[
            "--memo-file-dir-path",
            "./game",
            "--user-name",
            "bob",
            "--log-path",
            "./logs",
            "--chronosort-path",
            "./chrono",
        ]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        assert_eq!(
            parsed.launch_mode_selection,
            LaunchModeSelection::RunInlineInCurrentTerminal
        );
        assert_eq!(parsed.memo_files_directory_path.as_deref(), Some("./game"));
        assert_eq!(parsed.local_user_name.as_deref(), Some("bob"));
        assert_eq!(parsed.logging_directory_path.as_deref(), Some("./logs"));
        assert_eq!(
            parsed.chrono_sort_temp_directory_path.as_deref(),
            Some("./chrono")
        );
    }

    #[test]
    fn two_launch_modes_is_an_error() {
        let argv_vector = make_fake_argv(&["-nt", "-tv"]);
        let parse_outcome = parse_command_line_arguments(&argv_vector);
        assert!(parse_outcome.is_err(), "two launch modes must be rejected");
    }

    #[test]
    fn unknown_flag_is_an_error() {
        let argv_vector = make_fake_argv(&["--not-a-real-flag"]);
        let parse_outcome = parse_command_line_arguments(&argv_vector);
        assert!(parse_outcome.is_err(), "unknown flag must be rejected");
    }

    #[test]
    fn value_taking_flag_without_value_is_an_error() {
        let argv_vector = make_fake_argv(&["--user-name"]);
        let parse_outcome = parse_command_line_arguments(&argv_vector);
        assert!(
            parse_outcome.is_err(),
            "value-taking flag without value must be rejected"
        );
    }

    #[test]
    fn missing_real_run_inputs_is_a_validation_error() {
        let argv_vector = make_fake_argv(&["--user-name", "bob"]);
        let parsed = parse_command_line_arguments(&argv_vector).expect("parse must succeed");
        let resolution_outcome = resolve_real_run_inputs(&parsed);
        assert!(
            resolution_outcome.is_err(),
            "partial inputs must fail validation"
        );
    }

    #[test]
    fn child_argv_strips_short_new_terminal_flag() {
        let argv_vector = make_fake_argv(&["-nt", "--demo-test"]);
        let child_argv =
            build_child_argument_vector_for_relaunch(&argv_vector, RelaunchKind::NewTerminalWindow);
        assert_eq!(child_argv, vec!["--demo-test".to_string()]);
    }

    #[test]
    fn child_argv_strips_long_tmux_vertical_flag() {
        let argv_vector = make_fake_argv(&[
            "--tmux-split-vertical",
            "--user-name",
            "bob",
            "--memo-file-dir-path",
            "./g",
        ]);
        let child_argv =
            build_child_argument_vector_for_relaunch(&argv_vector, RelaunchKind::TmuxVerticalSplit);
        assert_eq!(
            child_argv,
            vec![
                "--user-name".to_string(),
                "bob".to_string(),
                "--memo-file-dir-path".to_string(),
                "./g".to_string(),
            ]
        );
    }

    #[test]
    fn child_argv_does_not_strip_unrelated_launch_flags() {
        // If somehow both `-nt` and `-tv` were present, parsing would
        // already have failed, but the stripper itself should only
        // remove the flag matching its kind. Test that property
        // directly.
        let argv_vector = make_fake_argv(&["-nt", "-tv"]);
        let child_argv =
            build_child_argument_vector_for_relaunch(&argv_vector, RelaunchKind::NewTerminalWindow);
        assert_eq!(child_argv, vec!["-tv".to_string()]);
    }
}
