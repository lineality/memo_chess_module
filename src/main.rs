//! # memo_chess_tui — Smoke Test Binary
//!
//! ## Project Context
//!
//! `memo_chess_tui` is a terminal-displayed chess game where player moves
//! arrive as TOML memo files in a flat directory. This binary is, for the
//! current development phase, a *smoke test* only: it constructs the initial
//! board state and writes an ASCII rendering to standard output. No file
//! reading, no clocks, no refresh loop, no Q&A — those concerns belong to
//! later development phases that will compose with the chess rules engine
//! exposed by `memo_chess_tui_module`.
//!
//! ## Why a Smoke-Test Binary?
//!
//! The chess rules engine is the foundation of the entire project. Before
//! adding any layer on top (file ingestion, timing, TUI refresh), the engine
//! must compile cleanly, produce a correct starting position, and render it
//! recognizably. This binary provides a one-command verification of that
//! foundation: `cargo run` should print a standard chess starting position.
//!
//! ## Production-Rule Adherence in This Binary
//!
//! - No use of `println!` or `print!` macros (they may allocate and panic on
//!   broken pipes). All output goes through `std::io::Write::write_all` on a
//!   stack-allocated buffer.
//! - No `unwrap`, no `expect`, no `panic!`. I/O errors are caught and the
//!   process exits with a non-zero status code via `std::process::exit` —
//!   which is graceful program termination, not a panic.
//! - No heap allocation. The render buffer is a fixed-size stack array.

// ============================================================================
// main.rs — Memo-Chess MVP-1 Demo
// ============================================================================
//
// ## Project Context
//
// Minimal demo driver that exercises the complete memo_chess pipeline:
//
//   1. Hard-code a game directory path and local user name.
//   2. Call `q_and_a_setup_bootstrap` to gather configuration from memos.
//   3. Call `create_initial_dungeon_master_state` to build initial game state.
//   4. Call `run_memochess_dungeon_master_loop` to play the game.
//
// The demo assumes:
//   - The game directory exists and is writable.
//   - Player memos appear in that directory (typically written by a
//     test harness or manual player input during the game).
//   - The terminal is available for output.
//
// ## Bootstrap Directories
//
// The bootstrap layer requires three filesystem paths:
//   1. `game_files_directory_path`: where player memos live.
//   2. `chrono_sort_temp_directory_path`: working directory for the
//      chrono-index module (must NOT be inside the game directory).
//   3. `memochess_logging_directory_path`: where error logs and game
//      logs are written.
//
// All three are created on-demand by the underlying modules (or
// already exist). The demo chooses temporary directory paths that
// are safe and will not interfere with other processes.
//
// ## Error Handling
//
// If `q_and_a_setup_bootstrap` returns an error, the demo prints a
// message and exits with a non-zero code. This is appropriate for a
// CLI tool: bootstrap failures are show-stoppers.
//
// Once the game loop is running, all failures are handled by the
// game loop itself (per project policy: no panic, handle all errors).
// The demo has no further error handling to do.
//
// ## Memory & Panic Policy
//
// This is demo code for testing and development. It may use heap
// freely for command-line parsing, path construction, etc. The
// constraint "no heap in production code paths" applies to the
// library modules (Sections 1–64), not to `main.rs`.
//
// No panics in the game logic itself. If `main.rs` encounters
// an error it cannot recover from, it may print and exit() with
// a non-zero code; this is appropriate for a CLI entry point.

mod memo_chess_tui_module;
use std::path::Path;

fn main() {
    // ─────────────────────────────────────────────────────────────
    // Step 1: Define the filesystem paths for this game instance.
    // ─────────────────────────────────────────────────────────────

    // // The directory where player memos (TOML files) are written.
    // // Using a subdirectory of the system temp directory for the demo.
    // let game_files_directory = std::env::temp_dir().join("memo_chess_demo_game");

    // // The working directory for the chrono-index module.
    // // Must NOT be inside the game directory.
    // let chrono_sort_temp_directory = std::env::temp_dir().join("memo_chess_demo_chrono");

    // // The directory where bootstrap error logs and game logs are written.
    // let memochess_logging_directory = std::env::temp_dir().join("memo_chess_demo_logs");

    //  ─────────────────────────────────────────
    //   Select Game: Four Inputs to start game
    //  ─────────────────────────────────────────
    let game_files_directory = Path::new("./test_game_files_1_prawntakespawn").to_path_buf(); // pawn takes pawn
    // let game_files_directory = Path::new("./test_game_files_2_whitetime_test").to_path_buf(); // testing white clock
    // let game_files_directory = Path::new("./test_game_files_4_whitetime_test2").to_path_buf(); // pawn takes pawn
    // let game_files_directory = Path::new("./test_game_files_3_foolmates").to_path_buf(); // pawn takes pawn

    let chrono_sort_temp_directory = Path::new("./test_chrono_temp").to_path_buf();
    let memochess_logging_directory = Path::new("./test_logs").to_path_buf();

    // IF the user is the black player: their terminal is 'inverted'
    // to so they see their board normally and do not have to play
    // upside-down.
    // The local user name. In a real deployment, this would come from
    // the application state or a command-line argument. For the demo,
    // hard-code it as "demo_player".
    // let local_user_name: &[u8] = b"ready_player";
    let local_user_name: &[u8] = b"bob";

    // ─────────────────────────────────────────────────────────────
    // Step 2: Bootstrap the game configuration.
    // ─────────────────────────────────────────────────────────────

    // The bootstrap function polls the game directory every 10 seconds
    // (per Section 59) until all required configuration memos have been
    // written by the players. It will print prompts to the terminal
    // guiding the user on what to provide.

    println!("═══════════════════════════════════════════════════════════");
    println!("Memo-Chess MVP-1 Demo");
    println!("═══════════════════════════════════════════════════════════");
    println!();
    println!("Game files directory:  {}", game_files_directory.display());
    println!(
        "Chrono-index temp dir: {}",
        chrono_sort_temp_directory.display()
    );
    println!(
        "Logging directory:     {}",
        memochess_logging_directory.display()
    );
    println!();
    println!(
        "Local user name: {}",
        String::from_utf8_lossy(local_user_name)
    );
    println!();
    println!("Waiting for bootstrap configuration...");
    println!();

    let config_result = memo_chess_tui_module::q_and_a_setup_bootstrap(
        &game_files_directory,
        local_user_name,
        &chrono_sort_temp_directory,
        &memochess_logging_directory,
    );

    let game_config = match config_result {
        Ok(config) => config,
        Err(bootstrap_error) => {
            eprintln!();
            eprintln!("Bootstrap failed: {:?}", bootstrap_error);
            eprintln!("Unable to start game. Please check configuration and try again.");
            std::process::exit(1);
        }
    };

    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!("Configuration complete. Starting game...");
    println!("═══════════════════════════════════════════════════════════");
    println!();

    // ─────────────────────────────────────────────────────────────
    // Step 3: Create the initial dungeon-master state.
    // ─────────────────────────────────────────────────────────────

    let initial_state = memo_chess_tui_module::create_initial_dungeon_master_state(game_config);

    // ─────────────────────────────────────────────────────────────
    // Step 4: Board Setup: get existing moves
    // ─────────────────────────────────────────────────────────────

    // Replay any existing moves from the directory (no sleep, no wall-clock).
    let replayed_state =
        memo_chess_tui_module::replay_existing_moves_from_chrono_index(initial_state);

    // ─────────────────────────────────────────────────────────────
    // Step 5: Run the game loop until the game ends.
    // ─────────────────────────────────────────────────────────────

    let final_state = memo_chess_tui_module::run_memochess_dungeon_master_loop(replayed_state);
    // let final_state = memo_chess_tui_module::run_memochess_dungeon_master_loop(initial_state);

    // ─────────────────────────────────────────────────────────────
    // Step 5: Print a summary of the outcome.
    // ─────────────────────────────────────────────────────────────

    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!("Game Over");
    println!("═══════════════════════════════════════════════════════════");
    println!();
    println!("Final game status: {:?}", final_state.board.game_status);
    println!("Final move number: {}", final_state.board.fullmove_number);
    println!();
    println!("Game files written to:  {}", game_files_directory.display());
    println!(
        "Logs written to:        {}",
        memochess_logging_directory.display()
    );
    println!();
}
