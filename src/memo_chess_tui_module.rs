//! # memo_chess_tui_module — Chess Rules Engine (Validation and State)
//!
//! ## Project Context
//!
//! This module is the *chess rules engine* foundation of the `memo_chess_tui`
//! project. The overall project displays a chess game in a terminal where
//! moves are delivered as TOML memo files written by players. Several layers
//! sit *on top of* this module in the final system:
//!
//! 1. **File ingestion layer** — scans a flat directory of TOML files,
//!    extracts `text_message`, `owner`, `updated_at_timestamp`, and passes
//!    notation byte slices to this module.
//! 2. **Player identity layer** — matches `owner` against configured player
//!    names; this module is *unaware* of player identity.
//! 3. **Clock layer** — tracks per-player and total time; this module is
//!    *unaware* of time.
//! 4. **TUI refresh layer** — periodically renders the current `BoardState`
//!    to the terminal; this module provides `format_board_state_as_ascii`
//!    but does not perform any terminal I/O.
//! 5. **Log layer** — appends a transcript to `chess_log.txt`; this module
//!    does not perform any file I/O.
//!
//! ## What This Module Does
//!
//! - Defines the chess data types (`BoardState`, `Piece`, `ChessMove`, etc.)
//! - Parses standard, algebraic, and long algebraic move notation
//! - Generates all legal moves for the side to move
//! - Validates a parsed move against the legal move set
//! - Applies a validated move to produce a new `BoardState`
//! - Detects check, checkmate, and stalemate
//! - Renders an ASCII representation of the board
//!
//! ## What This Module Does NOT Do
//!
//! - No file I/O of any kind
//! - No timekeeping or clock management
//! - No player identity matching
//! - No threefold repetition enforcement (per project spec)
//! - No 50-move rule enforcement (the `halfmove_clock` is *tracked* but not
//!   used as a termination condition)
//! - No terminal I/O (callers handle that)
//!
//! ## Board Indexing Convention
//!
//! Squares are indexed `0..=63` where:
//! - Index `0` = a1, index `7` = h1
//! - Index `8` = a2, index `15` = h2
//! - Index `56` = a8, index `63` = h8
//! - Formula: `index = rank * 8 + file`, where file `0` = 'a' and rank `0` = '1'
//!
//! This places White's back rank at the low indices and Black's back rank at
//! the high indices, which aligns with the natural ordering of ranks (1, 2,
//! 3, ..., 8) and avoids any per-color flipping during move generation.
//!
//! ## State Representation
//!
//! `BoardState` is fully `Copy`. All state is fixed-size and stack-allocated.
//! `apply_chess_move_to_state` takes `&BoardState` and returns a new owned
//! `BoardState` — moves never mutate their input. This makes the engine
//! trivially safe to use, easy to test, and impossible to corrupt by a
//! partially-applied move.
//!
//! ## Error Policy
//!
//! All fallible functions return `Result<T, MoveValidationError>`. The error
//! enum carries *no data* — every variant is a unit variant. This is
//! deliberate:
//!
//! - Production builds must not leak diagnostic information (file paths,
//!   user input fragments, internal state). A unit-variant enum is
//!   inherently incapable of carrying such data.
//! - Test builds use `Debug` derivation to print the variant name, which is
//!   sufficient for diagnosis without ever embedding user data.
//!
//! Callers wishing to log failures format the variant via `{:?}` *outside*
//! this module, at the file or TUI layer, where logging policy lives.
//!
//! ## Memory Policy
//!
//! No heap allocation occurs in any production code path of this module.
//! No `Vec`, no `String`, no `Box`, no `format!`. The legal move list is a
//! stack-allocated fixed-capacity buffer (`LegalMovesForCurrentTurn`).
//!
//! ## Panic Policy
//!
//! No `unwrap`, no `expect`, no `panic!`, no `assert!` in production code
//! paths. `debug_assert!` is used under `#[cfg(all(debug_assertions,
//! not(test)))]` for invariant checking during development. `assert!` is
//! used only inside `#[cfg(test)]` test functions.

// ============================================================================
// SECTION 1: Constants
// ============================================================================

/// Number of squares on a chess board (8 × 8).
pub const BOARD_SQUARE_COUNT: usize = 64;

/// Number of files (columns) on a chess board: a, b, c, d, e, f, g, h.
pub const BOARD_FILE_COUNT: u8 = 8;

/// Number of ranks (rows) on a chess board: 1, 2, 3, 4, 5, 6, 7, 8.
pub const BOARD_RANK_COUNT: u8 = 8;

/// Maximum number of legal moves possible in any chess position.
///
/// ## Sizing Rationale
///
/// The theoretical maximum number of legal moves in any reachable chess
/// position is 218 (a well-known result from chess programming literature).
/// We use 256 — the next power of two — to provide comfortable headroom
/// without wasting meaningful space. A `[ChessMove; 256]` is roughly 1 KiB,
/// which is acceptable on any modern stack.
///
/// If a position is ever encountered that exceeds this bound, move
/// generation will return an error rather than overflow. The bound is
/// checked at every push.
pub const MAX_LEGAL_MOVES_PER_POSITION: usize = 256;

// Square index constants for important squares. These avoid magic numbers
// throughout the codebase and document the indexing convention by example.

/// Index of square a1 (White's queenside rook home).
pub const SQUARE_INDEX_A1: u8 = 0;
/// Index of square e1 (White's king home).
pub const SQUARE_INDEX_E1: u8 = 4;
/// Index of square h1 (White's kingside rook home).
pub const SQUARE_INDEX_H1: u8 = 7;
/// Index of square a8 (Black's queenside rook home).
pub const SQUARE_INDEX_A8: u8 = 56;
/// Index of square e8 (Black's king home).
pub const SQUARE_INDEX_E8: u8 = 60;
/// Index of square h8 (Black's kingside rook home).
pub const SQUARE_INDEX_H8: u8 = 63;

// ============================================================================
// SECTION 2: Piece Types
// ============================================================================

/// Color of a chess piece, or equivalently the side a player is playing.
///
/// White always moves first. The `side_to_move` field of `BoardState`
/// alternates between these two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceColor {
    /// The White side. Moves first. Occupies ranks 1 and 2 in the starting
    /// position.
    White,
    /// The Black side. Moves second. Occupies ranks 7 and 8 in the starting
    /// position.
    Black,
}

impl PieceColor {
    /// Returns the opposing color. Used pervasively when alternating turns
    /// or computing whether a square is attacked by the enemy.
    ///
    /// This is a pure function with no failure mode.
    pub const fn opposite_color(self) -> PieceColor {
        match self {
            PieceColor::White => PieceColor::Black,
            PieceColor::Black => PieceColor::White,
        }
    }
}

/// Kind of chess piece, independent of color.
///
/// The variant ordering is not significant; we never rely on numeric
/// ordering of these variants in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    /// King. One per side. Castling and check rules apply.
    King,
    /// Queen. Sliding piece combining rook and bishop movement.
    Queen,
    /// Rook. Sliding piece along ranks and files.
    Rook,
    /// Bishop. Sliding piece along diagonals.
    Bishop,
    /// Knight. Jumping piece in an L-shape; not blocked by intervening pieces.
    Knight,
    /// Pawn. Forward movement, diagonal capture, en passant, promotion,
    /// double-push from starting rank.
    Pawn,
}

/// A chess piece: color and kind combined.
///
/// Stored in `BoardState.board_squares` as `Option<Piece>` where `None`
/// represents an empty square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Which side owns this piece.
    pub piece_color: PieceColor,
    /// What kind of piece this is.
    pub piece_kind: PieceKind,
}

// ============================================================================
// SECTION 3: Castling Rights
// ============================================================================

/// The four castling-rights flags maintained as part of `BoardState`.
///
/// Each flag is `true` if and only if the corresponding castle is *still
/// possible in principle* — that is, neither the king nor the relevant rook
/// has moved or been captured. A `true` flag does NOT mean castling is
/// legal *right now*; legality also requires path clearance and the king
/// not moving through, into, or out of check, which are checked at move
/// generation time.
///
/// All four flags start `true` in the initial position and only ever
/// transition to `false`. They never revert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastlingRights {
    /// White may still castle on the kingside (O-O).
    pub white_kingside: bool,
    /// White may still castle on the queenside (O-O-O).
    pub white_queenside: bool,
    /// Black may still castle on the kingside (O-O).
    pub black_kingside: bool,
    /// Black may still castle on the queenside (O-O-O).
    pub black_queenside: bool,
}

impl CastlingRights {
    /// Returns the castling rights at the start of a standard game: all
    /// four flags `true`.
    pub const fn initial_castling_rights() -> CastlingRights {
        CastlingRights {
            white_kingside: true,
            white_queenside: true,
            black_kingside: true,
            black_queenside: true,
        }
    }
}

// ============================================================================
// SECTION 4: Game Status
// ============================================================================

/// Current high-level state of the game.
///
/// Updated by `apply_chess_move_to_state` after each move. The TUI layer
/// reads this to decide whether to keep accepting moves and what to
/// display in the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    /// The game is in progress; the side to move has at least one legal
    /// move and is not in checkmate.
    Playing,
    /// Both players have agreed to a draw (via successive "draw" commands).
    /// Not produced by `apply_chess_move_to_state`; produced by the
    /// non-move command handling at a higher layer.
    Draw,
    /// The side to move has no legal moves and is *not* in check.
    Stalemate,
    /// White has won (Black is in checkmate, or Black resigned).
    WhiteWon,
    /// Black has won (White is in checkmate, or White resigned).
    BlackWon,
}

// ============================================================================
// SECTION 5: Move Representation
// ============================================================================

/// Category of a chess move, distinguishing special cases that affect how
/// `apply_chess_move_to_state` updates the board beyond a simple from/to
/// transfer.
///
/// Plain captures are `Normal` — the captured piece is determined by what
/// sits on the destination square, no special category is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChessMoveCategory {
    /// Any move not requiring special handling: quiet moves and ordinary
    /// captures by any piece.
    Normal,
    /// A pawn advancing exactly two squares from its starting rank. Sets
    /// the `en_passant_target_square` on the resulting state.
    DoublePawnPush,
    /// A pawn capturing en passant. The captured pawn is not on the
    /// destination square; it is on the square the moving pawn passed.
    EnPassant,
    /// Castling on the king's side (O-O). The king moves two squares
    /// toward the h-file rook; the rook jumps to the square the king
    /// crossed.
    CastleKingside,
    /// Castling on the queen's side (O-O-O). The king moves two squares
    /// toward the a-file rook; the rook jumps to the square the king
    /// crossed.
    CastleQueenside,
    /// A pawn reaching the back rank and being replaced by a piece of the
    /// same color. The `promotion_piece_kind` field of `ChessMove`
    /// specifies which.
    Promotion,
}

/// A fully-resolved chess move ready to be applied to a `BoardState`.
///
/// This is the output of `resolve_parsed_move_to_legal_chess_move` and the
/// input to `apply_chess_move_to_state`. By the time a value of this type
/// exists, the move has been validated as legal in some `BoardState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChessMove {
    /// Source square index (0..=63). The piece being moved originates here.
    pub from_square_index: u8,
    /// Destination square index (0..=63). The piece ends here (except for
    /// `CastleKingside`/`CastleQueenside` where the destination is the
    /// king's destination, not the rook's; and `EnPassant` where the
    /// destination is the empty square the pawn lands on).
    pub to_square_index: u8,
    /// For a promotion move, the kind of piece the pawn becomes. `None`
    /// for all other move categories.
    pub promotion_piece_kind: Option<PieceKind>,
    /// The category of this move, determining special handling in
    /// `apply_chess_move_to_state`.
    pub move_category: ChessMoveCategory,
}

/// The intermediate result of parsing a move notation string, before
/// resolution against the current board state.
///
/// At this stage, the notation has been syntactically decoded but no
/// legality has been checked. The fields capture *what the player wrote*,
/// which may be ambiguous (e.g., "Nc3" when two knights can reach c3) or
/// illegal (e.g., "e5" when the e-pawn is not on e4 or e2). Resolution
/// against `BoardState` happens in
/// `resolve_parsed_move_to_legal_chess_move`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedMoveNotation {
    /// Which kind of piece is moving. Defaults to `Pawn` when notation
    /// omits a piece letter (e.g., "e4" implies a pawn move).
    pub piece_kind: PieceKind,
    /// File of the destination square (0..=7, where 0 = 'a' and 7 = 'h').
    pub destination_file: u8,
    /// Rank of the destination square (0..=7, where 0 = '1' and 7 = '8').
    pub destination_rank: u8,
    /// True if the notation indicated a capture (contained 'x'). Used as
    /// a sanity check during resolution: a notation claiming capture must
    /// resolve to a move that captures.
    pub is_capture: bool,
    /// Disambiguation by source file, when notation provides it
    /// (e.g., the 'a' in "Rac1" means "the rook on the a-file").
    pub disambiguation_source_file: Option<u8>,
    /// Disambiguation by source rank, when notation provides it
    /// (e.g., the '1' in "R1c3" means "the rook on rank 1").
    pub disambiguation_source_rank: Option<u8>,
    /// Promotion piece, when notation provides it (e.g., the 'Q' in
    /// "e8=Q"). Required when a pawn reaches the back rank.
    pub promotion_piece_kind: Option<PieceKind>,
    /// Explicit source file from long algebraic notation (e.g., the 'e' in
    /// "e2e4"). When present, `explicit_source_rank` is also present and
    /// the source square is fully determined.
    pub explicit_source_file: Option<u8>,
    /// Explicit source rank from long algebraic notation (e.g., the '2' in
    /// "e2e4").
    pub explicit_source_rank: Option<u8>,
    /// True if the notation was "O-O" or "0-0".
    pub is_castle_kingside: bool,
    /// True if the notation was "O-O-O" or "0-0-0".
    pub is_castle_queenside: bool,
}

// ============================================================================
// SECTION 6: Non-Move Player Commands
// ============================================================================

/// Commands a player may issue in lieu of a move via the `text_message`
/// field of a TOML memo.
///
/// These are *not* moves and do not pass through the chess validation
/// pipeline. The file ingestion / game-orchestration layer interprets
/// them according to the project rules (e.g., two consecutive "draw"
/// messages produce a Draw status; "resign" produces a Won status for
/// the opponent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonMovePlayerCommand {
    /// Player offers or accepts a draw.
    Draw,
    /// Player resigns. The opponent wins.
    Resign,
}

// ============================================================================
// SECTION 7: Legal Move List (Stack-Allocated)
// ============================================================================

/// A fixed-capacity stack-allocated container holding all legal moves for
/// the current turn.
///
/// ## Purpose
///
/// Move generation produces a set of legal moves. The "generate then
/// match" approach to notation resolution requires this set to be
/// available for lookup. Rather than using a heap-allocated `Vec`, this
/// module uses a fixed-size buffer of capacity `MAX_LEGAL_MOVES_PER_POSITION`
/// (256, comfortably above the theoretical maximum of 218).
///
/// ## Safety
///
/// `push_move` is bounds-checked. Attempting to push a 257th move returns
/// `Err`, which propagates as a `MoveValidationError::InternalMoveBufferFull`.
/// This can only occur if the theoretical maximum is wrong (it is not), so
/// in practice this is a defensive backstop.
#[derive(Debug, Clone, Copy)]
pub struct LegalMovesForCurrentTurn {
    /// Underlying storage. Only the first `moves_count` entries are
    /// meaningful; remaining entries are uninitialized-equivalent (filled
    /// with a sentinel constructed at initialization time).
    pub moves_buffer: [ChessMove; MAX_LEGAL_MOVES_PER_POSITION],
    /// Number of meaningful entries in `moves_buffer`.
    pub moves_count: u16,
}

impl LegalMovesForCurrentTurn {
    /// Constructs an empty legal-move list.
    ///
    /// The buffer is initialized with a sentinel `ChessMove` value (all
    /// zeros plus `Normal` category). This sentinel is never read because
    /// `moves_count` controls iteration; the initialization exists only to
    /// satisfy Rust's requirement that arrays be fully initialized and to
    /// avoid `MaybeUninit` complexity for this small, infrequent allocation.
    pub const fn new_empty_legal_moves_list() -> LegalMovesForCurrentTurn {
        let sentinel_chess_move = ChessMove {
            from_square_index: 0,
            to_square_index: 0,
            promotion_piece_kind: None,
            move_category: ChessMoveCategory::Normal,
        };
        LegalMovesForCurrentTurn {
            moves_buffer: [sentinel_chess_move; MAX_LEGAL_MOVES_PER_POSITION],
            moves_count: 0,
        }
    }

    /// Appends a move to the list, bounds-checked.
    ///
    /// Returns `Err(MoveValidationError::InternalMoveBufferFull)` if the
    /// buffer is at capacity. This is a defensive backstop; in any
    /// reachable chess position the count cannot exceed
    /// `MAX_LEGAL_MOVES_PER_POSITION`.
    pub fn push_move(&mut self, chess_move_to_add: ChessMove) -> Result<(), MoveValidationError> {
        let current_count_as_usize = self.moves_count as usize;
        if current_count_as_usize >= MAX_LEGAL_MOVES_PER_POSITION {
            return Err(MoveValidationError::InternalMoveBufferFull);
        }
        self.moves_buffer[current_count_as_usize] = chess_move_to_add;
        // Saturating add is defensive: even if somehow we got here at
        // u16::MAX, we would not wrap to 0.
        self.moves_count = self.moves_count.saturating_add(1);
        Ok(())
    }

    /// Returns a slice over the meaningful portion of the buffer.
    pub fn as_slice(&self) -> &[ChessMove] {
        let count_as_usize = self.moves_count as usize;
        // Defensive: clamp in case `moves_count` were somehow corrupted
        // beyond capacity (it cannot be via the public API, but
        // defense-in-depth is cheap here).
        let safe_count = if count_as_usize > MAX_LEGAL_MOVES_PER_POSITION {
            MAX_LEGAL_MOVES_PER_POSITION
        } else {
            count_as_usize
        };
        &self.moves_buffer[..safe_count]
    }
}

// ============================================================================
// SECTION 8: Error Type
// ============================================================================

/// All possible failure modes of the chess validation pipeline.
///
/// ## Design Note
///
/// Every variant is a unit variant. The enum carries no data. This is a
/// deliberate production-safety choice: error values returned from this
/// module cannot leak user input, board state, file paths, or any other
/// information. Callers that wish to log failures may use `{:?}`
/// formatting (which prints the variant name) at the layer where logging
/// policy lives — not inside this module.
///
/// ## Variant Naming
///
/// Variants begin with `Invalid` for legality failures and with
/// `Internal` for backstop conditions that should not occur in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveValidationError {
    /// The notation string could not be parsed (malformed syntax).
    InvalidNotation,
    /// The notation referenced a source square with no piece on it.
    InvalidSourceSquareEmpty,
    /// The notation referenced a source square containing an opponent's
    /// piece.
    InvalidSourceNotOwnPiece,
    /// A sliding-piece move was blocked by a piece between source and
    /// destination.
    InvalidDestinationObstructed,
    /// The destination square contains the player's own piece.
    InvalidDestinationFriendlyFire,
    /// The destination square is not reachable by the moving piece's
    /// movement rules (e.g., a bishop attempting an L-shape).
    InvalidDestinationIllegal,
    /// The move is geometrically valid but would leave the player's own
    /// king in check (or fails to escape an existing check).
    InvalidLeavesKingInCheck,
    /// Castling was attempted but the player's castling rights for that
    /// side have been forfeit.
    InvalidCastlingNoRights,
    /// Castling was attempted but squares between the king and rook are
    /// not empty.
    InvalidCastlingPathObstructed,
    /// Castling was attempted while the king is in check, would move
    /// through an attacked square, or would land on an attacked square.
    InvalidCastlingKingInCheck,
    /// An en passant move was specified but the en passant target square
    /// is not set, or the moving pawn is not adjacent to it.
    InvalidEnPassantConditions,
    /// A pawn reached the promotion rank without a promotion piece
    /// designation in the notation.
    InvalidPromotionRequired,
    /// A promotion designation was given on a non-pawn move.
    InvalidPromotionOnNonPawn,
    /// The notation matched zero legal moves in the current position.
    InvalidNoMatchingLegalMove,
    /// The notation matched two or more legal moves; the player must
    /// provide disambiguation (e.g., "Nbd2" instead of "Nd2").
    InvalidAmbiguousNotation,
    /// The game is not in `Playing` status; no further moves are accepted.
    InvalidGameAlreadyEnded,
    /// Defensive backstop: the legal-move buffer reached its capacity
    /// (256). Theoretically unreachable in any valid chess position.
    InternalMoveBufferFull,
    /// Defensive backstop: an internal index/rank/file fell outside the
    /// expected range. Theoretically unreachable via the public API.
    InternalIndexOutOfBounds,
    /// Promotion to king or pawn (`e8=K`, `e8=P`)
    InvalidPromotionPieceKind,
}

// ============================================================================
// SECTION 9: Board State
// ============================================================================

/// The complete state of a chess game at one point in time.
///
/// ## Immutability Pattern
///
/// `BoardState` derives `Copy`. Functions that "modify" the state in fact
/// take `&BoardState` and return a new owned `BoardState`. The original
/// is unchanged. This makes the engine impossible to corrupt by a
/// partially-applied move and trivially safe to use in any threading
/// model.
///
/// ## Field Ordering
///
/// Fields are ordered by conceptual importance: the board itself first,
/// then turn information, then the auxiliary state required to implement
/// special move rules (castling, en passant), then bookkeeping (move
/// counters, status).
#[derive(Debug, Clone, Copy)]
pub struct BoardState {
    /// The 64 squares of the board. `None` represents an empty square.
    /// Indexed per the module-level board indexing convention:
    /// `index = rank * 8 + file`.
    pub board_squares: [Option<Piece>; BOARD_SQUARE_COUNT],
    /// Whose turn it is to move.
    pub side_to_move: PieceColor,
    /// Castling rights for both players.
    pub castling_rights: CastlingRights,
    /// If the most recent move was a double pawn push, this holds the
    /// index of the square the pawn skipped over (the square an en
    /// passant capture would land on). `None` otherwise.
    pub en_passant_target_square: Option<u8>,
    /// The number of full moves played, starting at 1. Increments after
    /// each Black move.
    pub fullmove_number: u16,
    /// Half-moves since the last pawn move or capture. Tracked for the
    /// 50-move rule. NOT enforced as a termination condition in this
    /// project (per spec).
    pub halfmove_clock: u8,
    /// Current high-level game status (Playing, checkmate, stalemate).
    pub game_status: GameStatus,
}

// ============================================================================
// SECTION 10: Square Index Helpers
// ============================================================================

/// Converts a (file, rank) pair to a board square index.
///
/// File and rank are both `0..=7`. Returns `Err(InternalIndexOutOfBounds)`
/// for any out-of-range input. Defense-in-depth: callers should already
/// have validated bounds, but this function does not trust them.
pub fn square_index_from_file_and_rank(
    file_zero_to_seven: u8,
    rank_zero_to_seven: u8,
) -> Result<u8, MoveValidationError> {
    if file_zero_to_seven >= BOARD_FILE_COUNT || rank_zero_to_seven >= BOARD_RANK_COUNT {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }
    // Both values are in 0..=7, so the multiplication and addition fit
    // comfortably in u8 (max value 7*8 + 7 = 63).
    Ok(rank_zero_to_seven * BOARD_FILE_COUNT + file_zero_to_seven)
}

/// Extracts the file (0..=7) from a square index.
///
/// Returns `Err(InternalIndexOutOfBounds)` if the index exceeds 63.
pub fn file_from_square_index(square_index: u8) -> Result<u8, MoveValidationError> {
    if square_index >= BOARD_SQUARE_COUNT as u8 {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }
    Ok(square_index % BOARD_FILE_COUNT)
}

/// Extracts the rank (0..=7) from a square index.
///
/// Returns `Err(InternalIndexOutOfBounds)` if the index exceeds 63.
pub fn rank_from_square_index(square_index: u8) -> Result<u8, MoveValidationError> {
    if square_index >= BOARD_SQUARE_COUNT as u8 {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }
    Ok(square_index / BOARD_FILE_COUNT)
}

// ============================================================================
// SECTION 11: Initial Board State
// ============================================================================

/// Constructs the standard chess starting position.
///
/// ## Layout
///
/// ```text
///  8  r n b q k b n r       (rank index 7, indices 56..=63)
///  7  p p p p p p p p       (rank index 6, indices 48..=55)
///  6  . . . . . . . .
///  5  . . . . . . . .
///  4  . . . . . . . .
///  3  . . . . . . . .
///  2  P P P P P P P P       (rank index 1, indices 8..=15)
///  1  R N B Q K B N R       (rank index 0, indices 0..=7)
///
///     a b c d e f g h
/// ```
///
/// ## Initial Conditions
///
/// - White moves first.
/// - All four castling rights are intact.
/// - No en passant target.
/// - Fullmove number is 1.
/// - Halfmove clock is 0.
/// - Game status is `Playing`.
///
/// ## Failure Mode
///
/// None. This function is infallible by construction — all square placements
/// are compile-time constants and all fields are populated explicitly.
pub fn create_initial_board_state() -> BoardState {
    let mut board_squares_array: [Option<Piece>; BOARD_SQUARE_COUNT] = [None; BOARD_SQUARE_COUNT];

    // White back rank (rank index 0, indices 0..=7).
    board_squares_array[0] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Rook,
    });
    board_squares_array[1] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Knight,
    });
    board_squares_array[2] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Bishop,
    });
    board_squares_array[3] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Queen,
    });
    board_squares_array[4] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::King,
    });
    board_squares_array[5] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Bishop,
    });
    board_squares_array[6] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Knight,
    });
    board_squares_array[7] = Some(Piece {
        piece_color: PieceColor::White,
        piece_kind: PieceKind::Rook,
    });

    // White pawns (rank index 1, indices 8..=15).
    let mut white_pawn_index: usize = 8;
    while white_pawn_index <= 15 {
        board_squares_array[white_pawn_index] = Some(Piece {
            piece_color: PieceColor::White,
            piece_kind: PieceKind::Pawn,
        });
        white_pawn_index += 1;
    }

    // Black pawns (rank index 6, indices 48..=55).
    let mut black_pawn_index: usize = 48;
    while black_pawn_index <= 55 {
        board_squares_array[black_pawn_index] = Some(Piece {
            piece_color: PieceColor::Black,
            piece_kind: PieceKind::Pawn,
        });
        black_pawn_index += 1;
    }

    // Black back rank (rank index 7, indices 56..=63).
    board_squares_array[56] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Rook,
    });
    board_squares_array[57] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Knight,
    });
    board_squares_array[58] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Bishop,
    });
    board_squares_array[59] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Queen,
    });
    board_squares_array[60] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::King,
    });
    board_squares_array[61] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Bishop,
    });
    board_squares_array[62] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Knight,
    });
    board_squares_array[63] = Some(Piece {
        piece_color: PieceColor::Black,
        piece_kind: PieceKind::Rook,
    });

    BoardState {
        board_squares: board_squares_array,
        side_to_move: PieceColor::White,
        castling_rights: CastlingRights::initial_castling_rights(),
        en_passant_target_square: None,
        fullmove_number: 1,
        halfmove_clock: 0,
        game_status: GameStatus::Playing,
    }
}

// ============================================================================
// SECTION 12: ASCII Rendering
// ============================================================================

/// Returns the ASCII character used to display a given piece.
///
/// White pieces are uppercase: `K Q R B N P`.
/// Black pieces are lowercase: `k q r b n p`.
/// This matches standard chess literature and PGN conventions.
const fn ascii_character_for_piece(piece: Piece) -> u8 {
    match (piece.piece_color, piece.piece_kind) {
        (PieceColor::White, PieceKind::King) => b'K',
        (PieceColor::White, PieceKind::Queen) => b'Q',
        (PieceColor::White, PieceKind::Rook) => b'R',
        (PieceColor::White, PieceKind::Bishop) => b'B',
        (PieceColor::White, PieceKind::Knight) => b'N',
        (PieceColor::White, PieceKind::Pawn) => b'P',
        (PieceColor::Black, PieceKind::King) => b'k',
        (PieceColor::Black, PieceKind::Queen) => b'q',
        (PieceColor::Black, PieceKind::Rook) => b'r',
        (PieceColor::Black, PieceKind::Bishop) => b'b',
        (PieceColor::Black, PieceKind::Knight) => b'n',
        (PieceColor::Black, PieceKind::Pawn) => b'p',
    }
}

/// Writes one byte to the output buffer at the given position, returning
/// the new position. Returns `Err` if the buffer would overflow.
///
/// This tiny helper centralizes the bounds check that every byte-write in
/// the renderer requires. No-heap, no-panic.
fn write_byte_to_buffer(
    output_buffer: &mut [u8],
    current_position: usize,
    byte_to_write: u8,
) -> Result<usize, MoveValidationError> {
    if current_position >= output_buffer.len() {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }
    output_buffer[current_position] = byte_to_write;
    Ok(current_position + 1)
}

/// Writes a slice of bytes to the output buffer at the given position,
/// returning the new position. Returns `Err` if the buffer would overflow.
fn write_bytes_to_buffer(
    output_buffer: &mut [u8],
    current_position: usize,
    bytes_to_write: &[u8],
) -> Result<usize, MoveValidationError> {
    let new_position = current_position + bytes_to_write.len();
    if new_position > output_buffer.len() {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }
    output_buffer[current_position..new_position].copy_from_slice(bytes_to_write);
    Ok(new_position)
}

/// Renders the given `BoardState` as ASCII text into the provided buffer.
///
/// ## Output Format
///
/// ```text
///  8  r n b q k b n r
///  7  p p p p p p p p
///  6  . . . . . . . .
///  5  . . . . . . . .
///  4  . . . . . . . .
///  3  . . . . . . . .
///  2  P P P P P P P P
///  1  R N B Q K B N R
///
///     a b c d e f g h
/// ```
///
/// ## Arguments
///
/// - `state`: the board state to render.
/// - `render_from_white_view`: if true, rank 8 is at the top and rank 1
///   is at the bottom (standard White orientation). If false, the board
///   is flipped so rank 1 is at the top (Black's perspective).
/// - `output_buffer`: a caller-provided stack buffer to write into. No
///   allocation occurs.
///
/// ## Returns
///
/// On success, `Ok(bytes_written)`: the number of bytes filled at the
/// start of `output_buffer`. The caller writes `&output_buffer[..n]` to
/// the terminal.
///
/// On failure, `Err(MoveValidationError::InternalIndexOutOfBounds)` if
/// the buffer was too small. The caller should treat this as a
/// programming error (buffer-sizing bug) rather than a user error.
///
/// ## Project Note
///
/// This renderer outputs only the board grid and file labels. Status
/// lines (turn, time, etc.) are the responsibility of the TUI layer,
/// which will compose this board rendering with status text from the
/// clock, file ingestion, and game-status layers. Keeping this function
/// focused on the board alone keeps it stable as those upper layers
/// evolve.
pub fn format_board_state_as_ascii(
    state: &BoardState,
    render_from_white_view: bool,
    output_buffer: &mut [u8],
) -> Result<usize, MoveValidationError> {
    let mut write_position: usize = 0;

    // Iterate ranks. White view goes 7 down to 0; Black view goes 0 up to 7.
    // We use an explicit, bounded loop counter rather than a Rust range-rev
    // chain to keep the iteration order trivially auditable.
    let mut rank_iteration_step: u8 = 0;
    while rank_iteration_step < BOARD_RANK_COUNT {
        let actual_rank_index: u8 = if render_from_white_view {
            // Top row first: rank 7, then 6, ..., then 0.
            BOARD_RANK_COUNT - 1 - rank_iteration_step
        } else {
            // Top row first: rank 0, then 1, ..., then 7.
            rank_iteration_step
        };

        // Leading space + rank number + two spaces.
        write_position = write_byte_to_buffer(output_buffer, write_position, b' ')?;
        // Rank label: '1' + rank index. Always a single ASCII digit.
        let rank_label_byte: u8 = b'1' + actual_rank_index;
        write_position = write_byte_to_buffer(output_buffer, write_position, rank_label_byte)?;
        write_position = write_byte_to_buffer(output_buffer, write_position, b' ')?;
        write_position = write_byte_to_buffer(output_buffer, write_position, b' ')?;

        // Iterate files for this rank.
        let mut file_iteration_step: u8 = 0;
        while file_iteration_step < BOARD_FILE_COUNT {
            let actual_file_index: u8 = if render_from_white_view {
                file_iteration_step
            } else {
                // Flip files for Black view so the board mirrors correctly.
                BOARD_FILE_COUNT - 1 - file_iteration_step
            };

            let square_index =
                square_index_from_file_and_rank(actual_file_index, actual_rank_index)?;
            let square_contents = state.board_squares[square_index as usize];

            let character_for_this_square: u8 = match square_contents {
                Some(piece_here) => ascii_character_for_piece(piece_here),
                None => b'.',
            };

            write_position =
                write_byte_to_buffer(output_buffer, write_position, character_for_this_square)?;
            // Separating space after each square except possibly the last.
            write_position = write_byte_to_buffer(output_buffer, write_position, b' ')?;

            file_iteration_step += 1;
        }

        // End of rank: newline.
        write_position = write_byte_to_buffer(output_buffer, write_position, b'\n')?;
        rank_iteration_step += 1;
    }

    // Blank line and file labels.
    write_position = write_byte_to_buffer(output_buffer, write_position, b'\n')?;
    write_position = write_bytes_to_buffer(output_buffer, write_position, b"    ")?;

    let mut file_label_step: u8 = 0;
    while file_label_step < BOARD_FILE_COUNT {
        let actual_file_index: u8 = if render_from_white_view {
            file_label_step
        } else {
            BOARD_FILE_COUNT - 1 - file_label_step
        };
        let file_label_byte: u8 = b'a' + actual_file_index;
        write_position = write_byte_to_buffer(output_buffer, write_position, file_label_byte)?;
        write_position = write_byte_to_buffer(output_buffer, write_position, b' ')?;
        file_label_step += 1;
    }
    write_position = write_byte_to_buffer(output_buffer, write_position, b'\n')?;

    Ok(write_position)
}

// ============================================================================
// SECTION 13: Cargo Tests for Part 1
// ============================================================================

#[cfg(test)]
mod tests_part_1_data_types_and_initial_state {
    use super::*;

    /// Verifies that `opposite_color` is an involution on both colors.
    #[test]
    fn opposite_color_is_symmetric() {
        assert_eq!(PieceColor::White.opposite_color(), PieceColor::Black);
        assert_eq!(PieceColor::Black.opposite_color(), PieceColor::White);
        assert_eq!(
            PieceColor::White.opposite_color().opposite_color(),
            PieceColor::White
        );
    }

    /// Verifies the initial castling rights are all true.
    #[test]
    fn initial_castling_rights_all_true() {
        let rights = CastlingRights::initial_castling_rights();
        assert!(rights.white_kingside);
        assert!(rights.white_queenside);
        assert!(rights.black_kingside);
        assert!(rights.black_queenside);
    }

    /// Verifies the square index helpers round-trip correctly for every
    /// (file, rank) pair on the board.
    #[test]
    fn square_index_helpers_round_trip() {
        let mut rank_index: u8 = 0;
        while rank_index < BOARD_RANK_COUNT {
            let mut file_index: u8 = 0;
            while file_index < BOARD_FILE_COUNT {
                let computed_index = square_index_from_file_and_rank(file_index, rank_index)
                    .expect("test: in-range inputs must produce Ok");
                let recovered_file =
                    file_from_square_index(computed_index).expect("test: index < 64");
                let recovered_rank =
                    rank_from_square_index(computed_index).expect("test: index < 64");
                assert_eq!(recovered_file, file_index);
                assert_eq!(recovered_rank, rank_index);
                file_index += 1;
            }
            rank_index += 1;
        }
    }

    /// Verifies the known-square constants match the formula.
    #[test]
    fn known_square_constants_match_formula() {
        assert_eq!(
            SQUARE_INDEX_A1,
            square_index_from_file_and_rank(0, 0).expect("test: a1 must be computable")
        );
        assert_eq!(
            SQUARE_INDEX_E1,
            square_index_from_file_and_rank(4, 0).expect("test: e1 must be computable")
        );
        assert_eq!(
            SQUARE_INDEX_H1,
            square_index_from_file_and_rank(7, 0).expect("test: h1 must be computable")
        );
        assert_eq!(
            SQUARE_INDEX_A8,
            square_index_from_file_and_rank(0, 7).expect("test: a8 must be computable")
        );
        assert_eq!(
            SQUARE_INDEX_E8,
            square_index_from_file_and_rank(4, 7).expect("test: e8 must be computable")
        );
        assert_eq!(
            SQUARE_INDEX_H8,
            square_index_from_file_and_rank(7, 7).expect("test: h8 must be computable")
        );
    }

    /// Verifies that out-of-range inputs to the index helpers return errors.
    #[test]
    fn square_index_helpers_reject_out_of_range() {
        assert_eq!(
            square_index_from_file_and_rank(8, 0),
            Err(MoveValidationError::InternalIndexOutOfBounds)
        );
        assert_eq!(
            square_index_from_file_and_rank(0, 8),
            Err(MoveValidationError::InternalIndexOutOfBounds)
        );
        assert_eq!(
            square_index_from_file_and_rank(255, 255),
            Err(MoveValidationError::InternalIndexOutOfBounds)
        );
        assert_eq!(
            file_from_square_index(64),
            Err(MoveValidationError::InternalIndexOutOfBounds)
        );
        assert_eq!(
            rank_from_square_index(255),
            Err(MoveValidationError::InternalIndexOutOfBounds)
        );
    }

    /// Verifies that the initial board state has the correct global
    /// metadata.
    #[test]
    fn initial_board_state_global_metadata_is_correct() {
        let state = create_initial_board_state();
        assert_eq!(state.side_to_move, PieceColor::White);
        assert_eq!(state.en_passant_target_square, None);
        assert_eq!(state.fullmove_number, 1);
        assert_eq!(state.halfmove_clock, 0);
        assert_eq!(state.game_status, GameStatus::Playing);
        assert!(state.castling_rights.white_kingside);
        assert!(state.castling_rights.white_queenside);
        assert!(state.castling_rights.black_kingside);
        assert!(state.castling_rights.black_queenside);
    }

    /// Verifies that the initial board state has each piece in the correct
    /// starting square.
    #[test]
    fn initial_board_state_piece_placement_is_correct() {
        let state = create_initial_board_state();

        // White back rank.
        let expected_white_back_rank: [PieceKind; 8] = [
            PieceKind::Rook,
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Queen,
            PieceKind::King,
            PieceKind::Bishop,
            PieceKind::Knight,
            PieceKind::Rook,
        ];
        for file_index in 0..8u8 {
            let square_index =
                square_index_from_file_and_rank(file_index, 0).expect("test: rank 0 is valid");
            let piece_here = state.board_squares[square_index as usize]
                .expect("test: white back rank must be occupied");
            assert_eq!(piece_here.piece_color, PieceColor::White);
            assert_eq!(
                piece_here.piece_kind,
                expected_white_back_rank[file_index as usize]
            );
        }

        // White pawns on rank 2 (rank index 1).
        for file_index in 0..8u8 {
            let square_index =
                square_index_from_file_and_rank(file_index, 1).expect("test: rank 1 is valid");
            let piece_here = state.board_squares[square_index as usize]
                .expect("test: white pawn rank must be occupied");
            assert_eq!(piece_here.piece_color, PieceColor::White);
            assert_eq!(piece_here.piece_kind, PieceKind::Pawn);
        }

        // Empty middle ranks (rank indices 2, 3, 4, 5).
        for rank_index in 2..6u8 {
            for file_index in 0..8u8 {
                let square_index = square_index_from_file_and_rank(file_index, rank_index)
                    .expect("test: middle ranks are valid");
                assert!(
                    state.board_squares[square_index as usize].is_none(),
                    "middle rank square should be empty"
                );
            }
        }

        // Black pawns on rank 7 (rank index 6).
        for file_index in 0..8u8 {
            let square_index =
                square_index_from_file_and_rank(file_index, 6).expect("test: rank 6 is valid");
            let piece_here = state.board_squares[square_index as usize]
                .expect("test: black pawn rank must be occupied");
            assert_eq!(piece_here.piece_color, PieceColor::Black);
            assert_eq!(piece_here.piece_kind, PieceKind::Pawn);
        }

        // Black back rank (rank index 7).
        let expected_black_back_rank: [PieceKind; 8] = [
            PieceKind::Rook,
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Queen,
            PieceKind::King,
            PieceKind::Bishop,
            PieceKind::Knight,
            PieceKind::Rook,
        ];
        for file_index in 0..8u8 {
            let square_index =
                square_index_from_file_and_rank(file_index, 7).expect("test: rank 7 is valid");
            let piece_here = state.board_squares[square_index as usize]
                .expect("test: black back rank must be occupied");
            assert_eq!(piece_here.piece_color, PieceColor::Black);
            assert_eq!(
                piece_here.piece_kind,
                expected_black_back_rank[file_index as usize]
            );
        }
    }

    /// Verifies the legal-move list starts empty and accepts pushes up to
    /// capacity, then rejects further pushes with the documented error.
    #[test]
    fn legal_moves_list_bounds_check() {
        let mut moves_list = LegalMovesForCurrentTurn::new_empty_legal_moves_list();
        assert_eq!(moves_list.moves_count, 0);
        assert_eq!(moves_list.as_slice().len(), 0);

        let placeholder_move = ChessMove {
            from_square_index: 0,
            to_square_index: 1,
            promotion_piece_kind: None,
            move_category: ChessMoveCategory::Normal,
        };

        // Fill to capacity.
        let mut push_iteration: usize = 0;
        while push_iteration < MAX_LEGAL_MOVES_PER_POSITION {
            let push_result = moves_list.push_move(placeholder_move);
            assert!(
                push_result.is_ok(),
                "push within capacity must succeed at iteration {}",
                push_iteration
            );
            push_iteration += 1;
        }

        assert_eq!(
            moves_list.moves_count as usize,
            MAX_LEGAL_MOVES_PER_POSITION
        );
        assert_eq!(moves_list.as_slice().len(), MAX_LEGAL_MOVES_PER_POSITION);

        // One more push must fail with the documented error.
        let overflow_result = moves_list.push_move(placeholder_move);
        assert_eq!(
            overflow_result,
            Err(MoveValidationError::InternalMoveBufferFull)
        );
        // Count must not have advanced past capacity.
        assert_eq!(
            moves_list.moves_count as usize,
            MAX_LEGAL_MOVES_PER_POSITION
        );
    }
    /// Verifies the ASCII renderer produces the exact expected output for
    /// the starting position in White view.
    ///
    /// ## Format Reference
    ///
    /// This test pins the rendering format documented in the project
    /// specification. Each rank line begins with a single leading space,
    /// then the rank digit, then two spaces, then the eight square
    /// characters each followed by a single trailing space.
    ///
    /// ## Implementation Note on the Expected String
    ///
    /// The expected string is built with the `concat!` macro, which
    /// joins string literals at compile time with no runtime cost and
    /// (critically) no whitespace stripping. We avoid the `\` line-
    /// continuation escape because it consumes the leading whitespace of
    /// the next line, which would silently strip our intentional leading
    /// spaces.
    ///
    /// Any future change to the renderer must update this expected
    /// string deliberately.
    #[test]
    fn ascii_render_initial_position_white_view() {
        let state = create_initial_board_state();
        let mut render_buffer: [u8; 1024] = [0u8; 1024];
        let bytes_written = format_board_state_as_ascii(&state, true, &mut render_buffer)
            .expect("test: 1024 bytes is more than enough for the board");

        let rendered_text = std::str::from_utf8(&render_buffer[..bytes_written])
            .expect("test: renderer must produce valid UTF-8 (ASCII subset)");

        let expected_text = concat!(
            " 8  r n b q k b n r \n",
            " 7  p p p p p p p p \n",
            " 6  . . . . . . . . \n",
            " 5  . . . . . . . . \n",
            " 4  . . . . . . . . \n",
            " 3  . . . . . . . . \n",
            " 2  P P P P P P P P \n",
            " 1  R N B Q K B N R \n",
            "\n",
            "    a b c d e f g h \n",
        );

        assert_eq!(
            rendered_text, expected_text,
            "ASCII rendering of initial position (White view) must match the documented format"
        );
    }

    /// Verifies the ASCII renderer produces the correctly mirrored output
    /// for the starting position in Black view.
    ///
    /// ## File Mirroring Derivation
    ///
    /// In Black view, the file order in the output is `h g f e d c b a`
    /// (left-to-right). For rank 1 (White's back rank), the pieces shown
    /// are those on the squares h1, g1, f1, e1, d1, c1, b1, a1 in that
    /// order — which gives `R N B K Q B N R` (King appears to the left
    /// of Queen because e1 is shown before d1 in this mirroring).
    ///
    /// ## Implementation Note
    ///
    /// See the white-view test for an explanation of `concat!` usage.
    #[test]
    fn ascii_render_initial_position_black_view() {
        let state = create_initial_board_state();
        let mut render_buffer: [u8; 1024] = [0u8; 1024];
        let bytes_written = format_board_state_as_ascii(&state, false, &mut render_buffer)
            .expect("test: 1024 bytes is more than enough for the board");

        let rendered_text = std::str::from_utf8(&render_buffer[..bytes_written])
            .expect("test: renderer must produce valid UTF-8 (ASCII subset)");

        let expected_text = concat!(
            " 1  R N B K Q B N R \n",
            " 2  P P P P P P P P \n",
            " 3  . . . . . . . . \n",
            " 4  . . . . . . . . \n",
            " 5  . . . . . . . . \n",
            " 6  . . . . . . . . \n",
            " 7  p p p p p p p p \n",
            " 8  r n b k q b n r \n",
            "\n",
            "    h g f e d c b a \n",
        );

        assert_eq!(
            rendered_text, expected_text,
            "ASCII rendering of initial position (Black view) must match the mirrored format"
        );
    }

    /// Verifies the ASCII renderer returns an error rather than corrupting
    /// memory when given a buffer that is too small.
    ///
    /// This is a defense-in-depth check: in production the caller is
    /// expected to provide a sufficient buffer, but the renderer must
    /// refuse to write out of bounds under any circumstances.
    #[test]
    fn ascii_render_rejects_undersized_buffer() {
        let state = create_initial_board_state();

        // A 16-byte buffer cannot hold even one rendered rank.
        let mut tiny_buffer: [u8; 16] = [0u8; 16];
        let result = format_board_state_as_ascii(&state, true, &mut tiny_buffer);
        assert_eq!(
            result,
            Err(MoveValidationError::InternalIndexOutOfBounds),
            "renderer must refuse to write past the end of a tiny buffer"
        );

        // A zero-byte buffer is the extreme case.
        let mut zero_buffer: [u8; 0] = [];
        let zero_result = format_board_state_as_ascii(&state, true, &mut zero_buffer);
        assert_eq!(
            zero_result,
            Err(MoveValidationError::InternalIndexOutOfBounds),
            "renderer must refuse to write to a zero-length buffer"
        );
    }

    /// Verifies that `Piece` and `BoardState` are `Copy` by value, which is
    /// part of the documented design (immutable functional updates).
    ///
    /// If a future change accidentally adds a non-`Copy` field, this test
    /// will fail to compile, which is the desired early-warning behavior.
    #[test]
    fn copy_semantics_are_preserved() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Piece>();
        assert_copy::<CastlingRights>();
        assert_copy::<GameStatus>();
        assert_copy::<ChessMove>();
        assert_copy::<ChessMoveCategory>();
        assert_copy::<ParsedMoveNotation>();
        assert_copy::<NonMovePlayerCommand>();
        assert_copy::<BoardState>();
        assert_copy::<MoveValidationError>();
        assert_copy::<LegalMovesForCurrentTurn>();
    }
}
