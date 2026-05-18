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

mod memo_chess_tui_module;

use std::io::Write;

/// Size of the stack buffer used to render the ASCII board.
///
/// ## Sizing Rationale
///
/// The ASCII board rendering uses at most a few hundred bytes:
/// - 8 ranks × roughly 24 bytes per rank (rank label, spaces, 8 pieces with
///   separators, newline) = ~192 bytes
/// - file label line: ~24 bytes
/// - status lines (placeholder): ~256 bytes
///
/// A 1024-byte buffer provides comfortable headroom and remains tiny on the
/// stack. If the rendering ever requires more, `format_board_state_as_ascii`
/// will indicate truncation via its return value and we will increase this
/// constant — not silently overflow.
const ASCII_RENDER_BUFFER_SIZE: usize = 1024;

/// Process exit code indicating successful smoke-test rendering.
const EXIT_CODE_SUCCESS: i32 = 0;

/// Process exit code indicating an I/O failure when writing to standard output.
///
/// This is graceful exit, not a panic. The classic case is a broken pipe
/// (e.g., `cargo run | head -1`). In production binaries we never panic; we
/// exit cleanly with a diagnostic-free non-zero code.
const EXIT_CODE_IO_FAILURE: i32 = 1;

/// Process exit code indicating the ASCII renderer signaled an internal
/// problem (for example, an unexpectedly small buffer). Reserved for future
/// expansion of the renderer's error reporting.
const EXIT_CODE_RENDER_FAILURE: i32 = 2;

fn main() {
    // Construct the standard chess starting position. This call is
    // infallible by construction (it returns an owned, fully-populated
    // `BoardState` with no I/O and no fallible operations).
    let initial_state = memo_chess_tui_module::create_initial_board_state();

    // Render to a stack buffer. No heap allocation occurs.
    let mut render_buffer: [u8; ASCII_RENDER_BUFFER_SIZE] = [0u8; ASCII_RENDER_BUFFER_SIZE];
    let render_result = memo_chess_tui_module::format_board_state_as_ascii(
        &initial_state,
        true, // white_view: the smoke test renders from White's perspective
        &mut render_buffer,
    );

    let bytes_written = match render_result {
        Ok(count) => count,
        Err(_) => {
            // Production policy: no diagnostic data leakage. A non-zero exit
            // status is the signal; the operator can re-run under cargo test
            // for detail.
            std::process::exit(EXIT_CODE_RENDER_FAILURE);
        }
    };

    // Write the rendered slice to standard output. We acquire a locked
    // handle once and write the entire slice in a single call to minimize
    // partial-write concerns.
    let stdout_handle = std::io::stdout();
    let mut stdout_lock = stdout_handle.lock();
    let write_outcome = stdout_lock.write_all(&render_buffer[..bytes_written]);

    match write_outcome {
        Ok(()) => std::process::exit(EXIT_CODE_SUCCESS),
        Err(_) => std::process::exit(EXIT_CODE_IO_FAILURE),
    }
}
