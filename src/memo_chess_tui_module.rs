//! # memo_chess_tui_module — Chess Rules Engine (Validation and State)
//!
//! ## Project Context
//!
//! This module is the  `memo_chess_tui`
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

// ============================================================================
// SECTION 13: Notation Parsing — Pre-Screen / Normalize
// ============================================================================
/*
In parse_residue_with_one_digit, the "preamble" extends from the start of the
residue to one byte before the destination file letter. This means a notation
like Rac1 decomposes as: preamble = "ra", destination file = 'c', destination
rank = '1'. The preamble decoder then reads 'r' as Rook and 'a' as a
disambiguation file. This is consistent with standard chess notation and is
exercised by the parses_rook_disambiguation_file_rac1 test.
*/

/// Maximum length of normalized notation input.
///
/// ## Sizing Rationale
///
/// The longest legal notation byte sequence we accept, after stripping
/// whitespace and parentheses, is 9 bytes. Examples:
///
/// - `"Qa1xb2=Q"` — 8 bytes (piece + full source + capture + dest + promo)
/// - `"Nbg1xf3"`  — 7 bytes (piece + disambig + source + capture + dest)
/// - `"resign"`   — 6 bytes (non-move command)
/// - `"O-O-O"`    — 5 bytes (queenside castle)
///
/// The check suffix `+` and mate suffix `#` and annotations (`!`, `?`)
/// extend this slightly, but every legal form fits in 9 bytes. We use 9
/// as a hard cap and reject any input that exceeds it after pre-screening.
pub const NOTATION_NORMALIZED_BUFFER_BYTES: usize = 9;

/// Pre-screens and normalizes a raw notation byte slice.
///
/// ## Project Context
///
/// This is the first stage of the notation parsing pipeline. The
/// file-ingestion layer reads `text_message` from a TOML memo file and
/// passes the resulting byte slice here. This function:
///
/// 1. Strips cosmetic characters (whitespace, parentheses).
/// 2. Lowercases ASCII uppercase letters (so the rest of the pipeline
///    can compare bytes literally without case-handling everywhere).
/// 3. Rejects any byte not in the allowed character set.
/// 4. Enforces the 9-byte maximum length.
/// 5. Rejects empty input.
///
/// By placing this normalization at the front of the pipeline, the
/// downstream parsers (`parse_move_notation`, `parse_non_move_player_command`)
/// can assume a canonical, all-lowercase, no-whitespace, bounded-length
/// input slice with no further input validation needed.
///
/// ## Allowed Character Set (after lowercasing)
///
/// - Digits:  `0 1 2 3 4 5 6 7 8` (note: `9` is excluded)
/// - Symbols: `= - + # ! ?`
/// - Letters: `a b c d e f g h i k n o p q r s w x`
///
/// The letters `i`, `s`, `w` are included only because they appear in
/// the non-move commands `resign` and `draw`. They never appear in legal
/// chess notation.
///
/// ## Arguments
///
/// - `input`: the raw byte slice from the TOML memo (or any caller).
/// - `output_buffer`: a caller-provided fixed-size buffer of exactly
///   `NOTATION_NORMALIZED_BUFFER_BYTES` (9) bytes into which the
///   normalized output is written.
///
/// ## Returns
///
/// - `Some(length)`: success; the normalized bytes occupy
///   `output_buffer[0..length]`.
/// - `None`: rejected (empty, oversize, contained non-ASCII, or
///   contained a disallowed character).
///
/// ## Failure Modes
///
/// Returns `None` for any of:
/// - Empty input, or all-whitespace input.
/// - Input containing any byte `>= 128` (non-ASCII).
/// - Input containing any byte not in the allowed set above (after
///   lowercasing and after skipping whitespace/parens).
/// - Input whose post-stripping length exceeds `NOTATION_NORMALIZED_BUFFER_BYTES`.
///
/// ## Memory & Panic Policy
///
/// No heap allocation. No panics. Single pass over the input. Bounded
/// loop (the loop terminates after at most `input.len()` iterations,
/// and writes are bounds-checked against the output buffer size).
pub fn pre_screen_and_normalize_notation_input(
    input: &[u8],
    output_buffer: &mut [u8; NOTATION_NORMALIZED_BUFFER_BYTES],
) -> Option<u8> {
    let mut written_length: usize = 0;

    // Bounded loop: at most input.len() iterations.
    let mut input_cursor: usize = 0;
    while input_cursor < input.len() {
        let raw_byte = input[input_cursor];
        input_cursor += 1;

        // Skip cosmetic characters silently.
        match raw_byte {
            b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')' => continue,
            _ => {}
        }

        // Reject non-ASCII outright. The allowed-character check below
        // would also catch these, but checking explicitly here makes the
        // failure mode unambiguous.
        if raw_byte >= 128 {
            return None;
        }

        // Lowercase ASCII uppercase letters by adding 32.
        // `b'A' = 65`, `b'a' = 97`, so b'A' + 32 = b'a'.
        let normalized_byte: u8 = if raw_byte >= b'A' && raw_byte <= b'Z' {
            raw_byte + 32
        } else {
            raw_byte
        };

        // Verify the normalized byte is in the allowed set.
        if !is_byte_in_allowed_notation_set(normalized_byte) {
            return None;
        }

        // Bounds-check before writing.
        if written_length >= NOTATION_NORMALIZED_BUFFER_BYTES {
            return None;
        }
        output_buffer[written_length] = normalized_byte;
        written_length += 1;
    }

    // Reject empty result (input was empty or all whitespace/parens).
    if written_length == 0 {
        return None;
    }

    // Safe conversion: `written_length` is at most
    // `NOTATION_NORMALIZED_BUFFER_BYTES` (9), which fits in u8.
    Some(written_length as u8)
}

/// Returns true if the given byte (already lowercased) is in the allowed
/// notation character set.
///
/// This is a private helper used by `pre_screen_and_normalize_notation_input`.
/// It is a `const fn` so the compiler can optimize the membership check
/// into a direct match table.
const fn is_byte_in_allowed_notation_set(lowercased_byte: u8) -> bool {
    matches!(
        lowercased_byte,
        // Digits 0..=8 (9 excluded).
        b'0' | b'1' | b'2' | b'3' | b'4' | b'5' | b'6' | b'7' | b'8'
        // Symbols.
        | b'=' | b'-' | b'+' | b'#' | b'!' | b'?'
        // File letters a..=h.
        | b'a' | b'b' | b'c' | b'd' | b'e' | b'f' | b'g' | b'h'
        // Piece letters (k=King, q=Queen, r=Rook, n=Knight, p=Pawn;
        // b is already covered above as a file letter; bishop is
        // disambiguated contextually).
        | b'k' | b'n' | b'p' | b'q' | b'r'
        // Castling letter.
        | b'o'
        // Capture marker.
        | b'x'
        // Letters required only for `draw` and `resign`.
        | b'i' | b's' | b'w'
    )
}

// ============================================================================
// SECTION 14: Notation Parsing — parse_move_notation / parse_non_move_player_command
// ============================================================================

/// Parses a notation byte slice into a `ParsedMoveNotation`.
///
/// ## Project Context
///
/// This is the *syntactic* parser. It accepts the raw bytes of the
/// `text_message` field from a TOML memo and produces an intermediate
/// `ParsedMoveNotation` capturing what the player wrote. It does **not**
/// consult any board state and does **not** check legality.
///
/// The output is consumed downstream by
/// `resolve_parsed_move_to_legal_chess_move` (Part 4/5 of the project),
/// which matches the parsed notation against the set of legal moves in
/// the current position.
///
/// ## Accepted Notation Forms
///
/// - Pawn moves: `e4`, `exd5`, `e8=Q`, `exd8=Q`, `e8Q` (no separator)
/// - Piece moves: `Nf3`, `Bxc6`, `Rac1`, `R1c3`, `Qa1b2`, `Naxb4`
/// - Long algebraic: `e2e4`, `e2-e4`, `e4xd5`, `Ng1f3`, `Ng1-f3`, `Bf3xc6`
/// - Castling: `O-O`, `O-O-O`, `0-0`, `0-0-0` (also `OO`, `OOO` without dashes)
/// - Suffixes accepted and discarded: `+`, `#`, `!`, `?`, `!!`, `??`, `!?`, `?!`
///
/// All forms are accepted case-insensitively (pre-screen lowercases).
///
/// ## Returns
///
/// - `Ok(ParsedMoveNotation)` on syntactic success.
/// - `Err(MoveValidationError::InvalidNotation)` for any syntactic failure.
/// - `Err(MoveValidationError::InvalidPromotionPieceKind)` if the player
///   explicitly designated promotion to King or Pawn.
///
/// ## Failure Modes (all return `Err`)
///
/// - Pre-screen rejection (empty, oversize, illegal character).
/// - Malformed castling sequence.
/// - Wrong number of rank digits (0, or > 2) in non-castling residue.
/// - File letter out of `a..=h` range in destination or source position.
/// - Rank digit `0` or `9` used as a rank (digit `0` is only valid in castling).
/// - Trailing `=` with no piece letter (e.g., `e8=`).
/// - Promotion piece is king or pawn (e.g., `e8=K`, `e8=P`).
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Operates entirely on a stack `[u8; 9]` buffer.
pub fn parse_move_notation(input: &[u8]) -> Result<ParsedMoveNotation, MoveValidationError> {
    // Step 1: pre-screen and normalize.
    let mut normalized_buffer: [u8; NOTATION_NORMALIZED_BUFFER_BYTES] =
        [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
    let normalized_length =
        match pre_screen_and_normalize_notation_input(input, &mut normalized_buffer) {
            Some(length_value) => length_value as usize,
            None => return Err(MoveValidationError::InvalidNotation),
        };

    // Defensive: pre-screen guarantees length >= 1 and <= 9.
    if normalized_length == 0 || normalized_length > NOTATION_NORMALIZED_BUFFER_BYTES {
        return Err(MoveValidationError::InvalidNotation);
    }

    // Step 2: strip trailing annotation/check/mate markers (+, #, !, ?).
    let mut working_length = normalized_length;
    while working_length > 0 {
        let last_byte = normalized_buffer[working_length - 1];
        if last_byte == b'+' || last_byte == b'#' || last_byte == b'!' || last_byte == b'?' {
            working_length -= 1;
        } else {
            break;
        }
    }
    if working_length == 0 {
        return Err(MoveValidationError::InvalidNotation);
    }

    // The slice we now operate on is `normalized_buffer[..working_length]`.
    let residue = &normalized_buffer[..working_length];

    // Step 3: castling detection. A castling residue contains only
    // `o`, `0`, and `-` characters (after lowercasing).
    if is_residue_castling_only_characters(residue) {
        return parse_castling_residue(residue);
    }

    // Step 4: non-castling decoding by digit-position-first strategy.
    parse_non_castling_residue(residue)
}

/// Returns true if every byte in `residue` is one of `o`, `0`, or `-`.
///
/// Used to short-circuit castling detection before the more general
/// non-castling parser runs.
const fn is_residue_castling_only_characters(residue: &[u8]) -> bool {
    let mut scan_index: usize = 0;
    while scan_index < residue.len() {
        let current_byte = residue[scan_index];
        if current_byte != b'o' && current_byte != b'0' && current_byte != b'-' {
            return false;
        }
        scan_index += 1;
    }
    true
}

/// Parses a residue known to contain only `o`, `0`, `-`.
///
/// After dropping the dashes (which are purely cosmetic), the remaining
/// length determines castling side:
/// - Length 2 (`oo`, `00`, `o0`, `0o`) → kingside.
/// - Length 3 (`ooo`, etc.) → queenside.
/// - Anything else → `InvalidNotation`.
///
/// This lenient policy accepts `O-O`, `O-O-O`, `0-0`, `0-0-0`, and
/// minor variations like `OO`, `OOO`, mixed `0`/`O`. It does not accept
/// e.g. a single `O` or four+ `O`s.
fn parse_castling_residue(residue: &[u8]) -> Result<ParsedMoveNotation, MoveValidationError> {
    let mut letter_count: u8 = 0;
    let mut scan_index: usize = 0;
    while scan_index < residue.len() {
        let current_byte = residue[scan_index];
        if current_byte == b'o' || current_byte == b'0' {
            // Bounded saturating add; in practice letter_count cannot
            // exceed residue.len() which is at most 9.
            letter_count = letter_count.saturating_add(1);
        }
        scan_index += 1;
    }

    match letter_count {
        2 => Ok(make_parsed_castling_notation(true, false)),
        3 => Ok(make_parsed_castling_notation(false, true)),
        _ => Err(MoveValidationError::InvalidNotation),
    }
}

/// Constructs a `ParsedMoveNotation` representing a castling move.
const fn make_parsed_castling_notation(
    is_kingside: bool,
    is_queenside: bool,
) -> ParsedMoveNotation {
    ParsedMoveNotation {
        piece_kind: PieceKind::King,
        // Destination fields are unused for castling; set to a sentinel
        // value (0,0) and let the resolution layer recognize the
        // castling flags.
        destination_file: 0,
        destination_rank: 0,
        is_capture: false,
        disambiguation_source_file: None,
        disambiguation_source_rank: None,
        promotion_piece_kind: None,
        explicit_source_file: None,
        explicit_source_rank: None,
        is_castle_kingside: is_kingside,
        is_castle_queenside: is_queenside,
    }
}

/// Parses a non-castling notation residue using the digit-position-first
/// strategy documented in the module-level docs.
///
/// The residue at this point:
/// - Contains no whitespace, parens, or trailing annotation markers.
/// - Is all lowercase ASCII.
/// - Is in the allowed character set.
/// - Does NOT consist entirely of `o`/`0`/`-` (that case was handled
///   by `parse_castling_residue`).
///
/// The strategy:
/// 1. Locate all rank-digit positions (`1`..=`8`) in the residue.
/// 2. Branch on digit count: exactly 1 (destination only) or exactly 2
///    (source + destination, where the leftmost digit may be a
///    disambiguation rank rather than a source rank).
/// 3. Extract preamble bytes (before the source square or destination
///    square), which may contain an optional piece letter, optional
///    disambiguation character, and an optional `x` capture marker.
/// 4. Extract suffix bytes (after the destination square), which may
///    contain an optional promotion designation.
fn parse_non_castling_residue(residue: &[u8]) -> Result<ParsedMoveNotation, MoveValidationError> {
    // Locate digit positions. A residue of length <= 9 has at most 9
    // digit positions; bounded loop.
    let mut digit_position_buffer: [usize; NOTATION_NORMALIZED_BUFFER_BYTES] =
        [0usize; NOTATION_NORMALIZED_BUFFER_BYTES];
    let mut digit_count: usize = 0;
    let mut scan_index: usize = 0;
    while scan_index < residue.len() {
        let current_byte = residue[scan_index];
        // `0` is rejected here for non-castling notation: as a rank, the
        // only valid digits are 1..=8. (Pre-screen lets `0` through for
        // castling; that branch has already been taken.)
        if current_byte == b'0' {
            return Err(MoveValidationError::InvalidNotation);
        }
        if current_byte >= b'1' && current_byte <= b'8' {
            if digit_count >= NOTATION_NORMALIZED_BUFFER_BYTES {
                // Defensive backstop; cannot occur in practice.
                return Err(MoveValidationError::InvalidNotation);
            }
            digit_position_buffer[digit_count] = scan_index;
            digit_count += 1;
        }
        scan_index += 1;
    }

    if digit_count == 0 || digit_count > 2 {
        return Err(MoveValidationError::InvalidNotation);
    }

    if digit_count == 1 {
        return parse_residue_with_one_digit(residue, digit_position_buffer[0]);
    }

    // digit_count == 2
    parse_residue_with_two_digits(residue, digit_position_buffer[0], digit_position_buffer[1])
}

/// Parses a residue containing exactly one rank digit.
///
/// Layout:
///
/// ```text
///   [ preamble bytes ] [ destination_file ] [ destination_rank ] [ promotion suffix ]
/// ```
///
/// - The destination rank is the digit at `digit_index`.
/// - The destination file is the byte immediately preceding it.
/// - The preamble (everything before the destination file) may contain:
///     - An optional piece letter (`k`, `q`, `r`, `b`, `n`, `p`).
///     - An optional disambiguation character (file letter or rank
///       digit — but rank digit disambiguation is excluded here because
///       we already know there is only one digit and it is the
///       destination rank).
///     - An optional `x` capture marker.
/// - The suffix (everything after the destination rank) may be a
///   promotion designation: `=Q`, `Q`, etc.
fn parse_residue_with_one_digit(
    residue: &[u8],
    digit_index: usize,
) -> Result<ParsedMoveNotation, MoveValidationError> {
    // Defensive bounds: digit_index must be at least 1 (need a file
    // letter before it) and within the residue.
    if digit_index == 0 || digit_index >= residue.len() {
        return Err(MoveValidationError::InvalidNotation);
    }

    let destination_rank_byte = residue[digit_index];
    let destination_file_byte = residue[digit_index - 1];

    let destination_rank = match rank_index_from_digit_byte(destination_rank_byte) {
        Some(rank_value) => rank_value,
        None => return Err(MoveValidationError::InvalidNotation),
    };
    let destination_file = match file_index_from_letter_byte(destination_file_byte) {
        Some(file_value) => file_value,
        None => return Err(MoveValidationError::InvalidNotation),
    };

    // Preamble = residue[0..(digit_index - 1)].
    let preamble = &residue[..(digit_index - 1)];

    // Suffix = residue[(digit_index + 1)..].
    let suffix = &residue[(digit_index + 1)..];

    let (piece_kind_value, disambig_file_opt, disambig_rank_opt, is_capture_flag) =
        decode_preamble_bytes(preamble)?;

    let promotion_piece_kind_opt = decode_promotion_suffix_bytes(suffix)?;

    Ok(ParsedMoveNotation {
        piece_kind: piece_kind_value,
        destination_file,
        destination_rank,
        is_capture: is_capture_flag,
        disambiguation_source_file: disambig_file_opt,
        disambiguation_source_rank: disambig_rank_opt,
        promotion_piece_kind: promotion_piece_kind_opt,
        explicit_source_file: None,
        explicit_source_rank: None,
        is_castle_kingside: false,
        is_castle_queenside: false,
    })
}

/// Parses a residue containing exactly two rank digits.
///
/// Two cases distinguished by the byte immediately before the *leftmost*
/// digit:
///
/// **Case A: byte before leftmost digit is a file letter `a..=h`.**
/// This is long-algebraic notation. Layout:
///
/// ```text
///   [ preamble bytes ] [ source_file ] [ source_rank ] [ optional separator(s) ]
///   [ destination_file ] [ destination_rank ] [ promotion suffix ]
/// ```
///
/// The preamble may contain an optional piece letter and an optional
/// disambiguation character. The separators between the source square
/// and destination square may be `-` (cosmetic) and/or `x` (capture).
///
/// **Case B: byte before leftmost digit is NOT a file letter.**
/// The leftmost digit is a disambiguation rank. Layout:
///
/// ```text
///   [ piece letter? ] [ disambig_rank (= leftmost digit) ] [ optional `x` ]
///   [ destination_file ] [ destination_rank ] [ promotion suffix ]
/// ```
fn parse_residue_with_two_digits(
    residue: &[u8],
    left_digit_index: usize,
    right_digit_index: usize,
) -> Result<ParsedMoveNotation, MoveValidationError> {
    // Defensive ordering check.
    if left_digit_index >= right_digit_index {
        return Err(MoveValidationError::InvalidNotation);
    }
    // Destination file must immediately precede the right digit.
    if right_digit_index == 0 {
        return Err(MoveValidationError::InvalidNotation);
    }

    let destination_file_byte = residue[right_digit_index - 1];
    let destination_rank_byte = residue[right_digit_index];

    let destination_rank = match rank_index_from_digit_byte(destination_rank_byte) {
        Some(rank_value) => rank_value,
        None => return Err(MoveValidationError::InvalidNotation),
    };
    let destination_file = match file_index_from_letter_byte(destination_file_byte) {
        Some(file_value) => file_value,
        None => return Err(MoveValidationError::InvalidNotation),
    };

    let left_digit_rank = match rank_index_from_digit_byte(residue[left_digit_index]) {
        Some(rank_value) => rank_value,
        None => return Err(MoveValidationError::InvalidNotation),
    };

    // Classify by the byte immediately to the left of the leftmost digit.
    let has_byte_before_left_digit = left_digit_index > 0;
    let byte_before_left_digit_opt: Option<u8> = if has_byte_before_left_digit {
        Some(residue[left_digit_index - 1])
    } else {
        None
    };

    let suffix = &residue[(right_digit_index + 1)..];
    let promotion_piece_kind_opt = decode_promotion_suffix_bytes(suffix)?;

    if let Some(byte_before_left) = byte_before_left_digit_opt {
        if let Some(source_file_value) = file_index_from_letter_byte(byte_before_left) {
            // Case A: long algebraic.
            // The bytes between the source rank digit and the destination
            // file byte are optional separators (`-`, `x`).
            // Destination file is at `right_digit_index - 1`.
            // Source rank digit is at `left_digit_index`.
            let separator_slice =
                if right_digit_index >= 1 && (right_digit_index - 1) > (left_digit_index + 1) {
                    &residue[(left_digit_index + 1)..(right_digit_index - 1)]
                } else {
                    // Empty separator region (source square is directly
                    // followed by destination square, e.g. `e2e4`).
                    &residue[0..0]
                };

            let mut is_capture_flag = false;
            let mut separator_scan: usize = 0;
            while separator_scan < separator_slice.len() {
                let separator_byte = separator_slice[separator_scan];
                match separator_byte {
                    b'-' => {} // cosmetic, discard
                    b'x' => is_capture_flag = true,
                    _ => return Err(MoveValidationError::InvalidNotation),
                }
                separator_scan += 1;
            }

            // The preamble (bytes before the source file letter) may
            // contain an optional piece letter and an optional
            // disambiguation character.
            let preamble_end_index = left_digit_index - 1; // position of source file letter
            let preamble = &residue[..preamble_end_index];

            // For long-algebraic forms, the preamble decoder must not
            // emit a capture marker (`x` would have to appear before the
            // source square, which is not a legal position for it). We
            // call the standard preamble decoder and combine its
            // capture flag with the one we found in the separator region.
            let (piece_kind_value, disambig_file_opt, disambig_rank_opt, preamble_capture_flag) =
                decode_preamble_bytes(preamble)?;

            let combined_capture_flag = is_capture_flag || preamble_capture_flag;

            return Ok(ParsedMoveNotation {
                piece_kind: piece_kind_value,
                destination_file,
                destination_rank,
                is_capture: combined_capture_flag,
                disambiguation_source_file: disambig_file_opt,
                disambiguation_source_rank: disambig_rank_opt,
                promotion_piece_kind: promotion_piece_kind_opt,
                explicit_source_file: Some(source_file_value),
                explicit_source_rank: Some(left_digit_rank),
                is_castle_kingside: false,
                is_castle_queenside: false,
            });
        }
    }

    // Case B: leftmost digit is a disambiguation rank.
    // Preamble = residue[..left_digit_index] is the piece letter (optional).
    // Between left_digit_index and (right_digit_index - 1): optional `x`.
    let preamble = &residue[..left_digit_index];
    let between_slice = &residue[(left_digit_index + 1)..(right_digit_index - 1)];

    let mut is_capture_flag = false;
    let mut between_scan: usize = 0;
    while between_scan < between_slice.len() {
        let between_byte = between_slice[between_scan];
        match between_byte {
            b'x' => is_capture_flag = true,
            _ => return Err(MoveValidationError::InvalidNotation),
        }
        between_scan += 1;
    }

    let (piece_kind_value, disambig_file_opt, _ignored_disambig_rank, preamble_capture_flag) =
        decode_preamble_bytes(preamble)?;

    let combined_capture_flag = is_capture_flag || preamble_capture_flag;

    Ok(ParsedMoveNotation {
        piece_kind: piece_kind_value,
        destination_file,
        destination_rank,
        is_capture: combined_capture_flag,
        disambiguation_source_file: disambig_file_opt,
        // The leftmost digit is the disambiguation rank in case B.
        disambiguation_source_rank: Some(left_digit_rank),
        promotion_piece_kind: promotion_piece_kind_opt,
        explicit_source_file: None,
        explicit_source_rank: None,
        is_castle_kingside: false,
        is_castle_queenside: false,
    })
}

/// Decodes a preamble byte slice.
///
/// The preamble is everything that appears before a destination square
/// (or before a source square in long-algebraic notation). It may
/// contain:
///
/// - An optional leading piece letter (`k`, `q`, `r`, `b`, `n`, `p`).
///   - Note: a leading `b` is ambiguous between "bishop" and "file b".
///     This decoder treats a leading `b` as a *file letter (i.e.,
///     disambiguation)* if and only if the preamble is exactly one byte
///     long (e.g., `bxc4` produces preamble `"b"`, which is the b-file
///     pawn). Otherwise, a leading `b` followed by more bytes is treated
///     as the bishop piece letter (e.g., `bc4` preamble `"b"` would
///     conflict — see disambiguation logic below).
///   - In practice, the disambiguation rule is: if the preamble starts
///     with a piece letter `k q r n` (unambiguous piece letters), strip
///     it as the piece. If it starts with `p`, strip it as Pawn. If it
///     starts with `b`, examine the remaining preamble: if the remaining
///     bytes form a valid disambiguation set (file letter and/or rank
///     digit) plus optional `x`, then `b` is the Bishop piece letter;
///     otherwise `b` is a disambiguation file. We use a simple
///     length-based heuristic: a `b` followed by exactly one
///     disambiguation character (file letter, rank digit, or `x`) is
///     ambiguous; we resolve it as **bishop** by default (the more
///     common case in real play).
///
///     *Concretely for this parser:* any leading letter in
///     `{k, q, r, b, n, p}` is treated as a piece letter. So `bxc4`
///     produces preamble `"b"` which is Bishop (preamble length 1, just
///     the piece letter). To express "b-file pawn captures c4" the
///     player must write `bxc4` as well — and the resolution layer
///     handles this: if Bishop cannot reach c4 but the b-pawn can, the
///     legal-move match will fail and the layer can re-try as a pawn.
///
///     **However**, for this parser's level of responsibility (pure
///     syntax), we take the simplest rule: leading `b` is Bishop. If
///     this turns out to cause friction in real play, the resolution
///     layer can be enhanced to retry pawn interpretation.
///
/// - An optional disambiguation file letter (`a`..=`h`) and/or
///   disambiguation rank digit (`1`..=`8`). Only one of each.
/// - An optional `x` capture marker (must be the last byte of the
///   preamble if present).
///
/// ## Returns
///
/// `(piece_kind, disambig_file_opt, disambig_rank_opt, is_capture_flag)`.
///
/// Defaults if preamble is empty: `Pawn`, `None`, `None`, `false`.
fn decode_preamble_bytes(
    preamble: &[u8],
) -> Result<(PieceKind, Option<u8>, Option<u8>, bool), MoveValidationError> {
    let mut cursor: usize = 0;
    let mut piece_kind_value: PieceKind = PieceKind::Pawn;
    let mut disambig_file_opt: Option<u8> = None;
    let mut disambig_rank_opt: Option<u8> = None;
    let mut is_capture_flag: bool = false;

    // Step 1: optional leading piece letter.
    if cursor < preamble.len() {
        let first_byte = preamble[cursor];
        match first_byte {
            b'k' => {
                piece_kind_value = PieceKind::King;
                cursor += 1;
            }
            b'q' => {
                piece_kind_value = PieceKind::Queen;
                cursor += 1;
            }
            b'r' => {
                piece_kind_value = PieceKind::Rook;
                cursor += 1;
            }
            b'n' => {
                piece_kind_value = PieceKind::Knight;
                cursor += 1;
            }
            b'p' => {
                piece_kind_value = PieceKind::Pawn;
                cursor += 1;
            }
            b'b' => {
                // Ambiguous: bishop OR file-b disambiguation.
                // Resolve as Bishop if there is at least one more byte
                // after `b` that is NOT an `x` (i.e., something that
                // could be a destination-related continuation).
                // For `bxc4`-style: preamble is `b` only (length 1), so
                // there is no "more byte after b" — Bishop with no
                // disambiguation. For `bxc4` the preamble after the
                // destination scan is actually `bx`, which we handle as
                // Bishop + capture below.
                piece_kind_value = PieceKind::Bishop;
                cursor += 1;
            }
            _ => {
                // No piece letter; leave as Pawn.
            }
        }
    }

    // Step 2: optional disambiguation chars and capture marker.
    // The remaining preamble may contain at most: one file letter, one
    // rank digit, one `x` — in any order, with no other characters.
    while cursor < preamble.len() {
        let current_byte = preamble[cursor];
        cursor += 1;

        if current_byte == b'x' {
            if is_capture_flag {
                // Two `x` markers — invalid.
                return Err(MoveValidationError::InvalidNotation);
            }
            is_capture_flag = true;
            continue;
        }

        if let Some(file_value) = file_index_from_letter_byte(current_byte) {
            if disambig_file_opt.is_some() {
                return Err(MoveValidationError::InvalidNotation);
            }
            disambig_file_opt = Some(file_value);
            continue;
        }

        if let Some(rank_value) = rank_index_from_digit_byte(current_byte) {
            if disambig_rank_opt.is_some() {
                return Err(MoveValidationError::InvalidNotation);
            }
            disambig_rank_opt = Some(rank_value);
            continue;
        }

        // Anything else in the preamble is invalid.
        return Err(MoveValidationError::InvalidNotation);
    }

    Ok((
        piece_kind_value,
        disambig_file_opt,
        disambig_rank_opt,
        is_capture_flag,
    ))
}

/// Decodes a promotion suffix slice.
///
/// Accepted forms:
/// - Empty slice → `None` (no promotion).
/// - `=q`, `=r`, `=b`, `=n` → `Some(corresponding PieceKind)`.
/// - `q`, `r`, `b`, `n` (no `=`) → `Some(corresponding PieceKind)`.
///
/// Rejected forms:
/// - `=k`, `=p`, bare `k`, bare `p` → `InvalidPromotionPieceKind`.
/// - `=` alone (no piece letter after) → `InvalidNotation`.
/// - Any other byte content → `InvalidNotation`.
fn decode_promotion_suffix_bytes(suffix: &[u8]) -> Result<Option<PieceKind>, MoveValidationError> {
    if suffix.is_empty() {
        return Ok(None);
    }

    let piece_letter_byte: u8 = if suffix[0] == b'=' {
        // Form: `=X`.
        if suffix.len() != 2 {
            return Err(MoveValidationError::InvalidNotation);
        }
        suffix[1]
    } else {
        // Form: bare `X`.
        if suffix.len() != 1 {
            return Err(MoveValidationError::InvalidNotation);
        }
        suffix[0]
    };

    match piece_letter_byte {
        b'q' => Ok(Some(PieceKind::Queen)),
        b'r' => Ok(Some(PieceKind::Rook)),
        b'b' => Ok(Some(PieceKind::Bishop)),
        b'n' => Ok(Some(PieceKind::Knight)),
        b'k' | b'p' => Err(MoveValidationError::InvalidPromotionPieceKind),
        _ => Err(MoveValidationError::InvalidNotation),
    }
}

/// Converts a rank digit byte (`b'1'`..=`b'8'`) to a rank index (0..=7).
///
/// Returns `None` for any other byte, including `b'0'` and `b'9'`.
const fn rank_index_from_digit_byte(byte_value: u8) -> Option<u8> {
    if byte_value >= b'1' && byte_value <= b'8' {
        Some(byte_value - b'1')
    } else {
        None
    }
}

/// Converts a file letter byte (`b'a'`..=`b'h'`) to a file index (0..=7).
///
/// Returns `None` for any other byte. Assumes the byte has already been
/// lowercased by the pre-screener.
const fn file_index_from_letter_byte(byte_value: u8) -> Option<u8> {
    if byte_value >= b'a' && byte_value <= b'h' {
        Some(byte_value - b'a')
    } else {
        None
    }
}

/// Parses a notation byte slice as a non-move player command.
///
/// ## Project Context
///
/// Players may write `draw` or `resign` in the `text_message` field of
/// a TOML memo instead of a chess move. The file-ingestion layer should
/// call this function first; if it returns `None`, the layer should
/// then attempt `parse_move_notation`.
///
/// ## Returns
///
/// - `Some(NonMovePlayerCommand::Draw)` for input that normalizes to `"draw"`.
/// - `Some(NonMovePlayerCommand::Resign)` for input that normalizes to `"resign"`.
/// - `None` for anything else, including pre-screen rejection.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Bounded loop.
pub fn parse_non_move_player_command(input: &[u8]) -> Option<NonMovePlayerCommand> {
    let mut normalized_buffer: [u8; NOTATION_NORMALIZED_BUFFER_BYTES] =
        [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
    let normalized_length = pre_screen_and_normalize_notation_input(input, &mut normalized_buffer)?;

    let normalized_slice = &normalized_buffer[..(normalized_length as usize)];

    if normalized_slice == b"draw" {
        return Some(NonMovePlayerCommand::Draw);
    }
    if normalized_slice == b"resign" {
        return Some(NonMovePlayerCommand::Resign);
    }
    None
}

// ============================================================================
// SECTION 15: Tests for Notation Parsing
// ============================================================================

#[cfg(test)]
mod tests_pre_screen {
    //! Tests for `pre_screen_and_normalize_notation_input`.
    //!
    //! These tests verify acceptance/rejection at the pre-screen stage
    //! independent of any downstream parsing.

    use super::*;

    #[test]
    fn pre_screen_rejects_empty_input() {
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        assert_eq!(
            pre_screen_and_normalize_notation_input(b"", &mut output_buffer),
            None
        );
    }

    #[test]
    fn pre_screen_rejects_all_whitespace_input() {
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        assert_eq!(
            pre_screen_and_normalize_notation_input(b"   \t \r\n  ", &mut output_buffer),
            None
        );
    }

    #[test]
    fn pre_screen_rejects_oversize_input() {
        // 10 valid chars after stripping → reject.
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        assert_eq!(
            pre_screen_and_normalize_notation_input(b"abcdefghij", &mut output_buffer),
            None
        );
    }

    #[test]
    fn pre_screen_rejects_non_ascii_input() {
        let non_ascii_input: [u8; 3] = [b'e', 0xC3, b'4'];
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        assert_eq!(
            pre_screen_and_normalize_notation_input(&non_ascii_input, &mut output_buffer),
            None
        );
    }

    #[test]
    fn pre_screen_rejects_disallowed_punctuation() {
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        assert_eq!(
            pre_screen_and_normalize_notation_input(b"e4@", &mut output_buffer),
            None
        );
        assert_eq!(
            pre_screen_and_normalize_notation_input(b"e4/", &mut output_buffer),
            None
        );
    }

    #[test]
    fn pre_screen_accepts_and_lowercases_uppercase_letters() {
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        let length_opt = pre_screen_and_normalize_notation_input(b"E4", &mut output_buffer);
        assert_eq!(length_opt, Some(2));
        assert_eq!(&output_buffer[..2], b"e4");
    }

    #[test]
    fn pre_screen_strips_parentheses() {
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        let length_opt = pre_screen_and_normalize_notation_input(b"(e4)", &mut output_buffer);
        assert_eq!(length_opt, Some(2));
        assert_eq!(&output_buffer[..2], b"e4");
    }

    #[test]
    fn pre_screen_strips_internal_whitespace() {
        let mut output_buffer = [0u8; NOTATION_NORMALIZED_BUFFER_BYTES];
        let length_opt = pre_screen_and_normalize_notation_input(b" N f 3 ", &mut output_buffer);
        assert_eq!(length_opt, Some(3));
        assert_eq!(&output_buffer[..3], b"nf3");
    }
}

#[cfg(test)]
mod tests_pawn_moves {
    //! Tests for pawn move notation parsing.

    use super::*;

    #[test]
    fn parses_pawn_advance_e4() {
        let result = parse_move_notation(b"e4").expect("e4 should parse");
        assert_eq!(result.piece_kind, PieceKind::Pawn);
        assert_eq!(result.destination_file, 4); // 'e' - 'a' = 4
        assert_eq!(result.destination_rank, 3); // '4' - '1' = 3
        assert!(!result.is_capture);
        assert_eq!(result.disambiguation_source_file, None);
        assert_eq!(result.disambiguation_source_rank, None);
        assert_eq!(result.promotion_piece_kind, None);
    }

    #[test]
    fn parses_pawn_capture_exd5() {
        let result = parse_move_notation(b"exd5").expect("exd5 should parse");
        assert_eq!(result.piece_kind, PieceKind::Pawn);
        assert_eq!(result.destination_file, 3); // 'd'
        assert_eq!(result.destination_rank, 4); // '5'
        assert!(result.is_capture);
        assert_eq!(result.disambiguation_source_file, Some(4)); // 'e'
    }

    #[test]
    fn parses_pawn_promotion_e8_eq_q() {
        let result = parse_move_notation(b"e8=Q").expect("e8=Q should parse");
        assert_eq!(result.piece_kind, PieceKind::Pawn);
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 7);
        assert_eq!(result.promotion_piece_kind, Some(PieceKind::Queen));
    }

    #[test]
    fn parses_pawn_capture_with_promotion_exd8_eq_q() {
        let result = parse_move_notation(b"exd8=Q").expect("exd8=Q should parse");
        assert_eq!(result.piece_kind, PieceKind::Pawn);
        assert_eq!(result.destination_file, 3);
        assert_eq!(result.destination_rank, 7);
        assert!(result.is_capture);
        assert_eq!(result.disambiguation_source_file, Some(4)); // 'e'
        assert_eq!(result.promotion_piece_kind, Some(PieceKind::Queen));
    }

    #[test]
    fn parses_pawn_promotion_no_separator_e8q() {
        let result = parse_move_notation(b"e8Q").expect("e8Q should parse");
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 7);
        assert_eq!(result.promotion_piece_kind, Some(PieceKind::Queen));
    }

    #[test]
    fn rejects_promotion_to_king() {
        assert_eq!(
            parse_move_notation(b"e8=K"),
            Err(MoveValidationError::InvalidPromotionPieceKind)
        );
    }
}

#[cfg(test)]
mod tests_piece_moves {
    //! Tests for piece (non-pawn) move notation parsing.

    use super::*;

    #[test]
    fn parses_knight_nf3() {
        let result = parse_move_notation(b"Nf3").expect("Nf3 should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.destination_file, 5); // 'f'
        assert_eq!(result.destination_rank, 2); // '3'
    }

    #[test]
    fn parses_bishop_capture_bxc6() {
        let result = parse_move_notation(b"Bxc6").expect("Bxc6 should parse");
        assert_eq!(result.piece_kind, PieceKind::Bishop);
        assert_eq!(result.destination_file, 2); // 'c'
        assert_eq!(result.destination_rank, 5); // '6'
        assert!(result.is_capture);
    }

    #[test]
    fn parses_rook_rc1() {
        let result = parse_move_notation(b"Rc1").expect("Rc1 should parse");
        assert_eq!(result.piece_kind, PieceKind::Rook);
        assert_eq!(result.destination_file, 2);
        assert_eq!(result.destination_rank, 0);
    }

    #[test]
    fn parses_queen_qd1() {
        let result = parse_move_notation(b"Qd1").expect("Qd1 should parse");
        assert_eq!(result.piece_kind, PieceKind::Queen);
        assert_eq!(result.destination_file, 3);
        assert_eq!(result.destination_rank, 0);
    }

    #[test]
    fn parses_king_ke2() {
        let result = parse_move_notation(b"Ke2").expect("Ke2 should parse");
        assert_eq!(result.piece_kind, PieceKind::King);
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 1);
    }

    #[test]
    fn parses_rook_disambiguation_file_rac1() {
        let result = parse_move_notation(b"Rac1").expect("Rac1 should parse");
        assert_eq!(result.piece_kind, PieceKind::Rook);
        assert_eq!(result.disambiguation_source_file, Some(0)); // 'a'
        assert_eq!(result.destination_file, 2);
        assert_eq!(result.destination_rank, 0);
    }

    #[test]
    fn parses_rook_disambiguation_rank_r1c3() {
        let result = parse_move_notation(b"R1c3").expect("R1c3 should parse");
        assert_eq!(result.piece_kind, PieceKind::Rook);
        assert_eq!(result.disambiguation_source_rank, Some(0)); // '1'
        assert_eq!(result.destination_file, 2);
        assert_eq!(result.destination_rank, 2);
    }
}

#[cfg(test)]
mod tests_disambiguation {
    //! Tests for disambiguation parsing.

    use super::*;

    #[test]
    fn file_only_disambiguation() {
        let result = parse_move_notation(b"Nbd2").expect("Nbd2 should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.disambiguation_source_file, Some(1)); // 'b'
        assert_eq!(result.disambiguation_source_rank, None);
        assert_eq!(result.destination_file, 3); // 'd'
        assert_eq!(result.destination_rank, 1); // '2'
    }

    #[test]
    fn rank_only_disambiguation() {
        let result = parse_move_notation(b"N3d2").expect("N3d2 should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.disambiguation_source_file, None);
        assert_eq!(result.disambiguation_source_rank, Some(2)); // '3'
        assert_eq!(result.destination_file, 3);
        assert_eq!(result.destination_rank, 1);
    }

    #[test]
    fn full_square_disambiguation_qa1b2() {
        // "Qa1b2" is two-digit residue. Left digit '1' is preceded by
        // file letter 'a', so this is long algebraic: source a1 → b2.
        let result = parse_move_notation(b"Qa1b2").expect("Qa1b2 should parse");
        assert_eq!(result.piece_kind, PieceKind::Queen);
        assert_eq!(result.explicit_source_file, Some(0));
        assert_eq!(result.explicit_source_rank, Some(0));
        assert_eq!(result.destination_file, 1);
        assert_eq!(result.destination_rank, 1);
    }

    #[test]
    fn file_disambiguation_with_capture_naxb4() {
        let result = parse_move_notation(b"Naxb4").expect("Naxb4 should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.disambiguation_source_file, Some(0)); // 'a'
        assert!(result.is_capture);
        assert_eq!(result.destination_file, 1); // 'b'
        assert_eq!(result.destination_rank, 3); // '4'
    }
}

#[cfg(test)]
mod tests_long_algebraic {
    //! Tests for long-algebraic notation parsing.

    use super::*;

    #[test]
    fn parses_long_algebraic_e2e4() {
        let result = parse_move_notation(b"e2e4").expect("e2e4 should parse");
        assert_eq!(result.piece_kind, PieceKind::Pawn);
        assert_eq!(result.explicit_source_file, Some(4));
        assert_eq!(result.explicit_source_rank, Some(1));
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 3);
        assert!(!result.is_capture);
    }

    #[test]
    fn parses_long_algebraic_with_hyphen_e2_dash_e4() {
        let result = parse_move_notation(b"e2-e4").expect("e2-e4 should parse");
        assert_eq!(result.explicit_source_file, Some(4));
        assert_eq!(result.explicit_source_rank, Some(1));
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 3);
        assert!(!result.is_capture);
    }

    #[test]
    fn parses_long_algebraic_with_capture_e4xd5() {
        let result = parse_move_notation(b"e4xd5").expect("e4xd5 should parse");
        assert_eq!(result.explicit_source_file, Some(4));
        assert_eq!(result.explicit_source_rank, Some(3));
        assert_eq!(result.destination_file, 3);
        assert_eq!(result.destination_rank, 4);
        assert!(result.is_capture);
    }

    #[test]
    fn parses_long_algebraic_with_piece_ng1f3() {
        let result = parse_move_notation(b"Ng1f3").expect("Ng1f3 should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.explicit_source_file, Some(6)); // 'g'
        assert_eq!(result.explicit_source_rank, Some(0)); // '1'
        assert_eq!(result.destination_file, 5); // 'f'
        assert_eq!(result.destination_rank, 2); // '3'
    }

    #[test]
    fn parses_long_algebraic_with_piece_and_hyphen_ng1_dash_f3() {
        let result = parse_move_notation(b"Ng1-f3").expect("Ng1-f3 should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.explicit_source_file, Some(6));
        assert_eq!(result.explicit_source_rank, Some(0));
        assert_eq!(result.destination_file, 5);
        assert_eq!(result.destination_rank, 2);
    }

    #[test]
    fn parses_long_algebraic_with_piece_and_capture_bf3xc6() {
        let result = parse_move_notation(b"Bf3xc6").expect("Bf3xc6 should parse");
        assert_eq!(result.piece_kind, PieceKind::Bishop);
        assert_eq!(result.explicit_source_file, Some(5));
        assert_eq!(result.explicit_source_rank, Some(2));
        assert_eq!(result.destination_file, 2);
        assert_eq!(result.destination_rank, 5);
        assert!(result.is_capture);
    }
}

#[cfg(test)]
mod tests_castling {
    //! Tests for castling notation parsing.

    use super::*;

    #[test]
    fn parses_kingside_castle_letter_o() {
        let result = parse_move_notation(b"O-O").expect("O-O should parse");
        assert!(result.is_castle_kingside);
        assert!(!result.is_castle_queenside);
    }

    #[test]
    fn parses_queenside_castle_letter_o() {
        let result = parse_move_notation(b"O-O-O").expect("O-O-O should parse");
        assert!(!result.is_castle_kingside);
        assert!(result.is_castle_queenside);
    }

    #[test]
    fn parses_kingside_castle_digit_zero() {
        let result = parse_move_notation(b"0-0").expect("0-0 should parse");
        assert!(result.is_castle_kingside);
    }

    #[test]
    fn parses_queenside_castle_digit_zero() {
        let result = parse_move_notation(b"0-0-0").expect("0-0-0 should parse");
        assert!(result.is_castle_queenside);
    }

    #[test]
    fn parses_kingside_castle_with_check_suffix() {
        let result = parse_move_notation(b"O-O+").expect("O-O+ should parse");
        assert!(result.is_castle_kingside);
    }
}

#[cfg(test)]
mod tests_suffix_stripping {
    //! Tests verifying that `+`, `#`, `!`, `?` suffixes are accepted and
    //! discarded.

    use super::*;

    #[test]
    fn check_suffix_stripped_from_pawn_move() {
        let result = parse_move_notation(b"e4+").expect("e4+ should parse");
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 3);
    }

    #[test]
    fn mate_suffix_stripped_from_piece_move() {
        let result = parse_move_notation(b"Nf3#").expect("Nf3# should parse");
        assert_eq!(result.piece_kind, PieceKind::Knight);
        assert_eq!(result.destination_file, 5);
        assert_eq!(result.destination_rank, 2);
    }

    #[test]
    fn annotation_suffix_stripped_combined_markers() {
        let result = parse_move_notation(b"e4!?").expect("e4!? should parse");
        assert_eq!(result.destination_file, 4);
        assert_eq!(result.destination_rank, 3);
    }
}

#[cfg(test)]
mod tests_non_move_commands {
    //! Tests for `parse_non_move_player_command`.

    use super::*;

    #[test]
    fn parses_draw_lowercase() {
        assert_eq!(
            parse_non_move_player_command(b"draw"),
            Some(NonMovePlayerCommand::Draw)
        );
    }

    #[test]
    fn parses_resign_lowercase() {
        assert_eq!(
            parse_non_move_player_command(b"resign"),
            Some(NonMovePlayerCommand::Resign)
        );
    }

    #[test]
    fn parses_draw_uppercase() {
        assert_eq!(
            parse_non_move_player_command(b"DRAW"),
            Some(NonMovePlayerCommand::Draw)
        );
    }

    #[test]
    fn parses_resign_mixed_case() {
        assert_eq!(
            parse_non_move_player_command(b"Resign"),
            Some(NonMovePlayerCommand::Resign)
        );
    }
}

#[cfg(test)]
mod tests_rejection_cases {
    //! Tests for syntactic rejection of malformed notation.

    use super::*;

    #[test]
    fn rejects_pawn_on_impossible_rank_e9() {
        // Digit '9' is not in the allowed character set; the pre-screen
        // rejects it before parsing.
        assert_eq!(
            parse_move_notation(b"e9"),
            Err(MoveValidationError::InvalidNotation)
        );
    }

    #[test]
    fn rejects_file_out_of_range_i4() {
        // 'i' is in the allowed set (because of "resign") but it is not
        // a valid file letter. The parser rejects it at the
        // file-extraction step.
        assert_eq!(
            parse_move_notation(b"i4"),
            Err(MoveValidationError::InvalidNotation)
        );
    }

    #[test]
    fn rejects_promotion_to_king_e8_eq_k() {
        assert_eq!(
            parse_move_notation(b"e8=K"),
            Err(MoveValidationError::InvalidPromotionPieceKind)
        );
    }

    #[test]
    fn rejects_trailing_equals_with_no_piece_letter_e8_eq() {
        // Per project policy, this is `InvalidNotation` (syntactic
        // malformation), not `InvalidPromotionPieceKind`.
        assert_eq!(
            parse_move_notation(b"e8="),
            Err(MoveValidationError::InvalidNotation)
        );
    }

    #[test]
    fn rejects_interior_check_marker_e_plus_4() {
        // The `+` is only valid as a trailing suffix. In the interior
        // of the notation it leaves a malformed residue after suffix
        // stripping (suffix stripping only strips the trailing tail).
        // After stripping: residue is "e+4" → the `+` is in the
        // disallowed position; the parser fails at preamble decoding.
        assert_eq!(
            parse_move_notation(b"e+4"),
            Err(MoveValidationError::InvalidNotation)
        );
    }
}

// ============================================================================
// SECTION 20: Game Time — Data Type
// ============================================================================

/// Time-related state for one game.
///
/// ## Source of truth
///
/// `white_cumulative_seconds` and `black_cumulative_seconds` are the
/// authoritative record of thinking time used by each player.  They are
/// updated only by `process_move_timestamp_for_game_time` (on a normal move)
/// or by `process_non_move_command_timestamp_for_game_time` (on a confirmed
/// resignation or mutually accepted draw).
///
/// ## Whose clock is running
///
/// `GameTimeState` does NOT store "who is on the clock."  That information
/// lives on `BoardState::side_to_move`, which is the single source of truth
/// for whose turn it is.  Every time function that needs to know who is on
/// the clock either takes a `&BoardState` or an explicit `clock_owner_color`
/// parameter (see per-function documentation for which, and why).
///
/// ## Live display
///
/// `compute_player_time_remaining_seconds` is a pure function: it adds the
/// elapsed-since-last-normal-move to the stored cumulative on demand, using a
/// caller-supplied `current_unix_timestamp`.  Nothing in this struct ticks.
///
/// ## Pre-moves
///
/// A move whose timestamp is earlier than `last_normal_move_unix_timestamp`
/// is treated as a pre-move.  No time is charged.  No state is updated.
///
/// ## End of game
///
/// When a player's clock runs out, `timeflagged_player` is set to that
/// color.  This is the sole "game over by clock" signal — there is no
/// separate `is_finalized` boolean.  The game loop ends when the overall
/// game status (in `BoardState::game_status`) reflects game over, by any
/// cause (checkmate, stalemate, resignation, draw, or time flag observed by
/// the game-orchestration layer reading `timeflagged_player`).
///
/// ## Sizing
///
/// `u32` for per-player times supports up to ~136 years of thinking time per
/// player; this is comfortably sufficient.
#[derive(Debug, Clone, Copy)]
pub struct GameTimeState {
    // ── Configuration (set once at construction; never mutated) ───────────
    /// Maximum allowed thinking time per player in seconds.
    /// A player whose used time meets or exceeds this value has flagged.
    /// Example: a 10-minute game is `600`.
    pub max_time_per_player_seconds: u32,

    // ── Running totals (updated only on normal moves and end-game) ────────
    /// Seconds of thinking time White has definitively used.
    pub white_cumulative_seconds: u32,

    /// Seconds of thinking time Black has definitively used.
    pub black_cumulative_seconds: u32,

    // ── Clock reference points ────────────────────────────────────────────
    /// Unix timestamp (seconds) of the most recent *normal* move.
    ///
    /// This is the reference point for both the next move's charge
    /// calculation and the live elapsed-time display.
    ///
    /// `None` until the very first normal move has been processed (i.e.,
    /// White's first move when bootstrap arrives at it).
    pub last_normal_move_unix_timestamp: Option<u64>,

    /// Unix timestamp (seconds) of the very first move (game start).
    ///
    /// Used only for the total-elapsed-time display.  `None` until the
    /// first normal move has been processed.
    pub game_start_unix_timestamp: Option<u64>,

    // ── End-of-game signal ────────────────────────────────────────────────
    /// Set when a player's clock has run out.
    ///
    /// `None` while the game is ongoing or until a flag is observed.
    /// `Some(PieceColor::White)` means White flagged; Black wins.
    /// `Some(PieceColor::Black)` means Black flagged; White wins.
    pub timeflagged_player: Option<PieceColor>,
}

// ============================================================================
// SECTION 21: Game Time — Construction
// ============================================================================

impl GameTimeState {
    /// Create a `GameTimeState` ready for the start of a game.
    ///
    /// `max_time_per_player_seconds` is the per-player thinking-time budget,
    /// e.g. `600` for a 10-minute game.  No clock reference is established
    /// until the first move is processed.
    ///
    /// This is a `const fn` so it can be used in static contexts if needed.
    pub const fn new_initial_game_time_state(max_time_per_player_seconds: u32) -> GameTimeState {
        GameTimeState {
            max_time_per_player_seconds,
            white_cumulative_seconds: 0,
            black_cumulative_seconds: 0,
            last_normal_move_unix_timestamp: None,
            game_start_unix_timestamp: None,
            timeflagged_player: None,
        }
    }
}

// ============================================================================
// SECTION 22: Game Time — Internal Helper (charge time to one player)
// ============================================================================

/// Add `elapsed_seconds` to the cumulative time of `color`, with saturation.
///
/// Internal helper.  Centralizes the per-color match used by both the move
/// processor and the non-move-command processor.
fn add_to_cumulative_time_for_color(
    game_time_state: &mut GameTimeState,
    color: PieceColor,
    elapsed_seconds: u32,
) {
    match color {
        PieceColor::White => {
            game_time_state.white_cumulative_seconds = game_time_state
                .white_cumulative_seconds
                .saturating_add(elapsed_seconds);
        }
        PieceColor::Black => {
            game_time_state.black_cumulative_seconds = game_time_state
                .black_cumulative_seconds
                .saturating_add(elapsed_seconds);
        }
    }
}

/// Read the cumulative time used by `color`.
///
/// Internal helper.  Pure function.
fn read_cumulative_time_for_color(game_time_state: &GameTimeState, color: PieceColor) -> u32 {
    match color {
        PieceColor::White => game_time_state.white_cumulative_seconds,
        PieceColor::Black => game_time_state.black_cumulative_seconds,
    }
}

/// Compute the elapsed seconds between two unix timestamps, saturating
/// into `u32`.  If `current` is not greater than `previous`, returns 0.
///
/// Internal helper.  Pure function.  Saturating semantics defend against
/// malformed or out-of-range TOML timestamps.
fn elapsed_seconds_saturating_u32(previous_unix: u64, current_unix: u64) -> u32 {
    if current_unix <= previous_unix {
        return 0;
    }
    let diff_u64 = current_unix - previous_unix;
    if diff_u64 > u32::MAX as u64 {
        u32::MAX
    } else {
        diff_u64 as u32
    }
}

/// Set `timeflagged_player` if `color`'s cumulative time has met or exceeded
/// the per-player limit.  Idempotent: if already flagged, the existing flag
/// is preserved (the first observed flag wins).
///
/// Internal helper.
fn check_and_set_timeflagged_for_color(game_time_state: &mut GameTimeState, color: PieceColor) {
    if game_time_state.timeflagged_player.is_some() {
        return;
    }
    let used = read_cumulative_time_for_color(game_time_state, color);
    if used >= game_time_state.max_time_per_player_seconds {
        game_time_state.timeflagged_player = Some(color);
    }
}

// ============================================================================
// SECTION 23: Game Time — Move Timestamp Processing
// ============================================================================

/// Process a single move's timestamp and update `GameTimeState` accordingly.
///
/// ## When to call
///
/// Call this once per move, in chronological order (by `mtime`-sorted file
/// order, which the file-ingestion layer guarantees).  Used in both modes:
/// - **Bootstrap:** iterating historical move files to reconstruct game time.
/// - **Live loop:** when a newly-arrived TOML file yields a validated move.
///
/// ## Parameter: `clock_owner_color`
///
/// The color of the player whose clock was running at the moment this move
/// was issued — i.e., `side_to_move` of the `BoardState` **before** the move
/// is applied.  The caller is responsible for passing this correctly.  The
/// time module deliberately does not look at `BoardState` here, because at
/// move-processing time the caller already has a clear "pre-move" state on
/// hand and the contract is simpler with an explicit parameter than with a
/// borrow of `BoardState` that the caller would have to remember to provide
/// from the right side of `apply_chess_move_to_state`.
///
/// `clock_owner_color` is only read when this move is determined to be a
/// normal (non-pre-move) non-first move.
///
/// ## Three cases handled
///
/// 1. **First move** (`last_normal_move_unix_timestamp.is_none()`):
///    Sets `game_start_unix_timestamp` and `last_normal_move_unix_timestamp`
///    to `move_unix_timestamp`.  Charges no one any time.  By chess rules
///    this can only be White's first move.
///
/// 2. **Pre-move** (`move_unix_timestamp < last_normal_move_unix_timestamp`):
///    Does nothing.  No time charged, no reference updated.  The pre-move
///    was issued before the previous normal move's timestamp and therefore
///    cannot have consumed any thinking time relative to that reference.
///
/// 3. **Normal move** (`move_unix_timestamp >= last_normal_move_unix_timestamp`):
///    Charges `clock_owner_color` the elapsed seconds, updates
///    `last_normal_move_unix_timestamp`, and checks whether the charge
///    caused that player to flag (sets `timeflagged_player` if so).
///
/// ## Already flagged
///
/// If `timeflagged_player` is already set, this function returns immediately
/// without modifying anything.  The first observed flag wins.
///
/// ## No return value
///
/// All outcomes are visible by inspecting the mutated `GameTimeState`:
/// - First move: `game_start_unix_timestamp` becomes `Some(...)`.
/// - Normal move: cumulative time changed; `timeflagged_player` may become
///   `Some(...)`.
/// - Pre-move: nothing changes.
/// - Already flagged: nothing changes.
pub fn process_move_timestamp_for_game_time(
    game_time_state: &mut GameTimeState,
    clock_owner_color: PieceColor,
    move_unix_timestamp: u64,
) {
    // Guard: once flagged, no further charges happen.
    if game_time_state.timeflagged_player.is_some() {
        return;
    }

    // Case 1: first move.
    let last_ts = match game_time_state.last_normal_move_unix_timestamp {
        None => {
            game_time_state.game_start_unix_timestamp = Some(move_unix_timestamp);
            game_time_state.last_normal_move_unix_timestamp = Some(move_unix_timestamp);
            return;
        }
        Some(t) => t,
    };

    // Case 2: pre-move.
    if move_unix_timestamp < last_ts {
        return;
    }

    // Case 3: normal move.
    let elapsed = elapsed_seconds_saturating_u32(last_ts, move_unix_timestamp);
    add_to_cumulative_time_for_color(game_time_state, clock_owner_color, elapsed);
    game_time_state.last_normal_move_unix_timestamp = Some(move_unix_timestamp);

    // Check whether this charge caused the player to flag.
    check_and_set_timeflagged_for_color(game_time_state, clock_owner_color);
}

// ============================================================================
// SECTION 24: Game Time — Non-Move Command Timestamp Processing
// ============================================================================

/// Process a non-move player command's timestamp (resignation, mutually
/// accepted draw) and charge time accordingly.
///
/// ## When to call
///
/// Call this **once**, at the moment the orchestration layer decides the
/// game is ending because of a non-move player command:
/// - A confirmed resignation by the issuing player.
/// - A mutually accepted draw at the moment the second "draw" arrives.
///
/// The single `command_unix_timestamp` is the timestamp of the deciding
/// command — for a resignation, the timestamp of the resign file; for a
/// mutually accepted draw, the timestamp of the second (accepting) draw
/// file.
///
/// ## What it charges
///
/// At the moment the command is issued, exactly one player is on the clock
/// — namely `board_state.side_to_move`.  This function charges that player
/// the elapsed seconds from `last_normal_move_unix_timestamp` to
/// `command_unix_timestamp`.
///
/// The rationale, restated from project notes: if it takes a player five
/// minutes of staring at the position before resigning, those five minutes
/// are properly counted against their thinking time.
///
/// ## Edge cases
///
/// - **No moves yet** (`last_normal_move_unix_timestamp.is_none()`):
///   Nothing is charged.  No reference point exists.  The function returns
///   without modifying anything.  (This corresponds to a resignation before
///   the game has started, which is unusual but harmless.)
/// - **Command timestamp <= last reference:**  Treated as zero elapsed.  No
///   time is charged.  Saturating semantics; we do not assume timestamps
///   are well-ordered.
/// - **Already flagged:** Returns immediately, preserving the existing flag.
///
/// ## Effect on `timeflagged_player`
///
/// If the charge causes the player to exceed their time budget,
/// `timeflagged_player` is set.  This is unusual (the game is ending
/// anyway), but kept for consistency with `process_move_timestamp_for_game_time`.
///
/// ## After this call
///
/// `last_normal_move_unix_timestamp` is **not** updated by this function.
/// There is no further move processing to anchor against; updating the
/// reference would have no consumer.
pub fn process_non_move_command_timestamp_for_game_time(
    game_time_state: &mut GameTimeState,
    board_state: &BoardState,
    command_unix_timestamp: u64,
) {
    if game_time_state.timeflagged_player.is_some() {
        return;
    }

    let last_ts = match game_time_state.last_normal_move_unix_timestamp {
        None => return,
        Some(t) => t,
    };

    let clock_owner_color = board_state.side_to_move;
    let elapsed = elapsed_seconds_saturating_u32(last_ts, command_unix_timestamp);

    add_to_cumulative_time_for_color(game_time_state, clock_owner_color, elapsed);
    check_and_set_timeflagged_for_color(game_time_state, clock_owner_color);
}

// ============================================================================
// SECTION 25: Game Time — Live Flag Check
// ============================================================================

/// Check whether the player currently on the clock has flagged as of
/// `current_unix_timestamp`.  If so, set `timeflagged_player`.
///
/// ## When to call
///
/// Call this each refresh cycle of the game loop, after deciding that no
/// new move has arrived.  It is the live-clock equivalent of the flag check
/// done inside `process_move_timestamp_for_game_time`.
///
/// ## What it does
///
/// 1. If `timeflagged_player` is already `Some(_)`, returns it unchanged.
/// 2. Otherwise, computes how much live time `board_state.side_to_move` has
///    consumed since `last_normal_move_unix_timestamp` and compares the sum
///    (cumulative + live) against `max_time_per_player_seconds`.
/// 3. If the sum meets or exceeds the limit, sets `timeflagged_player` to
///    that color.
///
/// ## Returns
///
/// `Some(color)` if anyone has flagged (now or earlier), `None` otherwise.
///
/// ## What it does NOT do
///
/// - Does not charge cumulative time.  Cumulative time advances only via
///   the move processor or the non-move-command processor.  Live elapsed
///   time is *computed* against the cumulative reference; it is not
///   *committed* into cumulative until a move actually arrives.
/// - Does not check the other player.  Only the player on the clock can
///   flag from inactivity.
pub fn check_for_time_flag(
    game_time_state: &mut GameTimeState,
    board_state: &BoardState,
    current_unix_timestamp: u64,
) -> Option<PieceColor> {
    if let Some(c) = game_time_state.timeflagged_player {
        return Some(c);
    }

    let last_ts = match game_time_state.last_normal_move_unix_timestamp {
        None => return None, // game has not started; nothing to flag
        Some(t) => t,
    };

    let clock_owner_color = board_state.side_to_move;
    let cumulative = read_cumulative_time_for_color(game_time_state, clock_owner_color);
    let live_elapsed = elapsed_seconds_saturating_u32(last_ts, current_unix_timestamp);

    // total_used = cumulative + live_elapsed, saturating
    let total_used = cumulative.saturating_add(live_elapsed);

    if total_used >= game_time_state.max_time_per_player_seconds {
        game_time_state.timeflagged_player = Some(clock_owner_color);
        return Some(clock_owner_color);
    }

    None
}

// ============================================================================
// SECTION 26: Game Time — Pure Queries (Display)
// ============================================================================

/// Compute the time *remaining* for `for_color`, in seconds, as of
/// `current_unix_timestamp`.
///
/// ## Purpose
///
/// Display function for the TUI.  Pure: does not mutate any state.
///
/// ## Calculation
///
/// ```text
/// remaining = max_time_per_player
///           - cumulative_used(for_color)
///           - live_elapsed                  [only if for_color is on the clock]
/// ```
///
/// `live_elapsed` is `current_unix_timestamp - last_normal_move_unix_timestamp`,
/// computed only when `for_color == board_state.side_to_move`.  Saturating
/// arithmetic throughout; the result is always in `[0, max_time_per_player]`.
///
/// ## Behavior before the game starts
///
/// If `last_normal_move_unix_timestamp` is `None`, returns
/// `max_time_per_player_seconds` for both colors.
pub fn compute_player_time_remaining_seconds(
    game_time_state: &GameTimeState,
    board_state: &BoardState,
    for_color: PieceColor,
    current_unix_timestamp: u64,
) -> u32 {
    let cumulative = read_cumulative_time_for_color(game_time_state, for_color);

    let live_elapsed: u32 = if board_state.side_to_move == for_color {
        match game_time_state.last_normal_move_unix_timestamp {
            Some(last_ts) => elapsed_seconds_saturating_u32(last_ts, current_unix_timestamp),
            None => 0,
        }
    } else {
        0
    };

    let total_used = cumulative.saturating_add(live_elapsed);
    game_time_state
        .max_time_per_player_seconds
        .saturating_sub(total_used)
}

/// Total wall-clock seconds since the game's first move.
///
/// Returns `0` if the game has not started yet (i.e., no first move
/// processed).  Returns `u64` rather than `u32` because total elapsed
/// wall-clock time of an unattended TUI process is unbounded in principle,
/// whereas per-player thinking time is bounded by configuration.
///
/// Pure: does not mutate state.
pub fn compute_total_elapsed_seconds_since_game_start(
    game_time_state: &GameTimeState,
    current_unix_timestamp: u64,
) -> u64 {
    match game_time_state.game_start_unix_timestamp {
        None => 0,
        Some(start) => current_unix_timestamp.saturating_sub(start),
    }
}

// ============================================================================
// SECTION 27: Game Time — Time Display Formatting
// ============================================================================

/// Format `total_seconds` as decimal ASCII `"H:MM:SS"` into `output_buffer`.
///
/// Hours are written with no leading zero and use as many digits as needed
/// (1 for 0–9 hours, 2 for 10–99 hours, etc.).  Minutes and seconds are
/// always two digits, zero-padded.
///
/// ## Examples
///
/// |  input | output       | bytes |
/// |-------:|:-------------|------:|
/// |     0  | `0:00:00`    |   7   |
/// |     1  | `0:00:01`    |   7   |
/// |    75  | `0:01:15`    |   7   |
/// |   600  | `0:10:00`    |   7   |
/// |  3723  | `1:02:03`    |   7   |
/// | 36000  | `10:00:00`   |   8   |
///
/// ## Return value
///
/// `Ok(bytes_written)` on success.  `Err(MoveValidationError::
/// InternalIndexOutOfBounds)` if `output_buffer` is too small.  No partial
/// write occurs on error.
///
/// ## Minimum buffer size
///
/// A buffer of 8 bytes is sufficient for any input up to 99h59m59s (359999
/// seconds, almost 100 hours).  For chess games this is comfortably
/// sufficient; callers should size at 8 bytes minimum.  For pathological
/// inputs the function will succeed with larger buffers and fail cleanly
/// with smaller ones.
pub fn format_seconds_as_hms_into_buffer(
    total_seconds: u32,
    output_buffer: &mut [u8],
) -> Result<usize, MoveValidationError> {
    let hours_value = total_seconds / 3600;
    let minutes_value = (total_seconds % 3600) / 60;
    let seconds_value = total_seconds % 60;

    // First, compute how many bytes we will need so we can fail before
    // writing anything.  Hours-digit-count is at least 1.
    let hour_digit_count = count_decimal_digits_u32(hours_value);
    // Layout: <hour_digits> ":" "MM" ":" "SS"
    let total_bytes_needed = hour_digit_count + 1 + 2 + 1 + 2;

    if output_buffer.len() < total_bytes_needed {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }

    let mut write_position: usize = 0;

    // Hours (no leading zero, variable width)
    write_position +=
        write_u32_decimal_into_buffer(hours_value, &mut output_buffer[write_position..])?;

    // ":"
    output_buffer[write_position] = b':';
    write_position += 1;

    // MM (always two digits)
    output_buffer[write_position] = b'0' + (minutes_value / 10) as u8;
    output_buffer[write_position + 1] = b'0' + (minutes_value % 10) as u8;
    write_position += 2;

    // ":"
    output_buffer[write_position] = b':';
    write_position += 1;

    // SS (always two digits)
    output_buffer[write_position] = b'0' + (seconds_value / 10) as u8;
    output_buffer[write_position + 1] = b'0' + (seconds_value % 10) as u8;
    write_position += 2;

    Ok(write_position)
}

/// Count how many decimal digits are needed to represent `value`, with a
/// minimum of 1 (value 0 is one digit).  Internal helper.  Pure.
fn count_decimal_digits_u32(value: u32) -> usize {
    if value == 0 {
        return 1;
    }
    let mut remaining = value;
    let mut digits: usize = 0;
    while remaining > 0 {
        digits += 1;
        remaining /= 10;
    }
    digits
}

/// Write `value` as decimal ASCII into `output_buffer`, no leading zeros
/// (except that value 0 produces the single character `'0'`).
///
/// Returns `Ok(bytes_written)` on success or
/// `Err(MoveValidationError::InternalIndexOutOfBounds)` if the buffer is
/// too small.
///
/// Internal helper.  Used by `format_seconds_as_hms_into_buffer`.
fn write_u32_decimal_into_buffer(
    value: u32,
    output_buffer: &mut [u8],
) -> Result<usize, MoveValidationError> {
    // u32::MAX = 4_294_967_295 → 10 decimal digits maximum.
    let mut reverse_digits = [0u8; 10];
    let mut digit_count: usize = 0;
    let mut remaining = value;

    loop {
        if digit_count >= reverse_digits.len() {
            // Cannot happen for u32 (max 10 digits), but defensive.
            return Err(MoveValidationError::InternalIndexOutOfBounds);
        }
        reverse_digits[digit_count] = b'0' + (remaining % 10) as u8;
        digit_count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }

    if digit_count > output_buffer.len() {
        return Err(MoveValidationError::InternalIndexOutOfBounds);
    }

    for i in 0..digit_count {
        output_buffer[i] = reverse_digits[digit_count - 1 - i];
    }

    Ok(digit_count)
}

// ============================================================================
// SECTION 28: Game Time — Tests
// ============================================================================

#[cfg(test)]
mod game_time_tests {
    use super::*;

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// A minimal `BoardState` for time-only tests.  We do not exercise any
    /// chess logic in these tests; only `side_to_move` is read by the time
    /// functions.  The other fields are filled with reasonable defaults
    /// using the existing module's initial-state primitives if available.
    fn make_test_board_state(side_to_move: PieceColor) -> BoardState {
        BoardState {
            board_squares: [None; BOARD_SQUARE_COUNT],
            side_to_move,
            castling_rights: CastlingRights::initial_castling_rights(),
            en_passant_target_square: None,
            fullmove_number: 1,
            halfmove_clock: 0,
            game_status: GameStatus::Playing,
        }
    }

    fn new_ten_minute_game_time() -> GameTimeState {
        GameTimeState::new_initial_game_time_state(600)
    }

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn test_new_game_time_state_is_zeroed() {
        let s = new_ten_minute_game_time();
        assert_eq!(s.max_time_per_player_seconds, 600);
        assert_eq!(s.white_cumulative_seconds, 0);
        assert_eq!(s.black_cumulative_seconds, 0);
        assert_eq!(s.last_normal_move_unix_timestamp, None);
        assert_eq!(s.game_start_unix_timestamp, None);
        assert_eq!(s.timeflagged_player, None);
    }

    // ── First move ───────────────────────────────────────────────────────────

    #[test]
    fn test_first_move_sets_references_charges_nobody() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);

        assert_eq!(s.white_cumulative_seconds, 0);
        assert_eq!(s.black_cumulative_seconds, 0);
        assert_eq!(s.last_normal_move_unix_timestamp, Some(1_000));
        assert_eq!(s.game_start_unix_timestamp, Some(1_000));
        assert_eq!(s.timeflagged_player, None);
    }

    // ── Normal moves ─────────────────────────────────────────────────────────

    #[test]
    fn test_second_move_charges_correct_player() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        // Black was on the clock from t=1000 to t=1030 (30s)
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 1_030);

        assert_eq!(s.white_cumulative_seconds, 0);
        assert_eq!(s.black_cumulative_seconds, 30);
        assert_eq!(s.last_normal_move_unix_timestamp, Some(1_030));
    }

    #[test]
    fn test_alternating_normal_moves_accumulate() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0); // first move
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 10); // black used 10
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 25); // white used 15
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 60); // black used 35

        assert_eq!(s.white_cumulative_seconds, 15);
        assert_eq!(s.black_cumulative_seconds, 45);
    }

    #[test]
    fn test_same_timestamp_charges_zero() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 1_000);

        assert_eq!(s.black_cumulative_seconds, 0);
        assert_eq!(s.last_normal_move_unix_timestamp, Some(1_000));
    }

    // ── Pre-moves ────────────────────────────────────────────────────────────

    #[test]
    fn test_premove_does_nothing() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        // Black "moved" at t=999, earlier than the reference
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 999);

        assert_eq!(s.black_cumulative_seconds, 0);
        assert_eq!(s.last_normal_move_unix_timestamp, Some(1_000));
    }

    #[test]
    fn test_premove_then_white_move_charges_from_original_reference() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 999); // pre-move
        // White plays at t=1020.  Note: in chess this is unusual because
        // black's "pre-move" has not actually been committed in board terms;
        // however, from the time-module perspective black's clock was running.
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 1_020);

        // Charge attributed to clock owner (Black) for 1000→1020 = 20s
        assert_eq!(s.black_cumulative_seconds, 20);
        assert_eq!(s.last_normal_move_unix_timestamp, Some(1_020));
    }

    #[test]
    fn test_multiple_consecutive_premoves_do_nothing() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 500);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 600);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 700);

        assert_eq!(s.white_cumulative_seconds, 0);
        assert_eq!(s.black_cumulative_seconds, 0);
        assert_eq!(s.last_normal_move_unix_timestamp, Some(1_000));
    }

    // ── Flag detection on move ───────────────────────────────────────────────

    #[test]
    fn test_flag_set_when_move_pushes_over_limit() {
        let mut s = GameTimeState::new_initial_game_time_state(30);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 35);

        assert_eq!(s.timeflagged_player, Some(PieceColor::Black));
        assert_eq!(s.black_cumulative_seconds, 35);
    }

    #[test]
    fn test_exactly_at_limit_flags() {
        let mut s = GameTimeState::new_initial_game_time_state(30);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 30);

        assert_eq!(s.timeflagged_player, Some(PieceColor::Black));
    }

    #[test]
    fn test_one_under_limit_does_not_flag() {
        let mut s = GameTimeState::new_initial_game_time_state(30);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 29);

        assert_eq!(s.timeflagged_player, None);
    }

    #[test]
    fn test_after_flag_further_moves_are_ignored() {
        let mut s = GameTimeState::new_initial_game_time_state(30);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 35);
        let snapshot_black = s.black_cumulative_seconds;
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 40);
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 100);

        assert_eq!(s.timeflagged_player, Some(PieceColor::Black));
        assert_eq!(s.black_cumulative_seconds, snapshot_black);
        assert_eq!(s.white_cumulative_seconds, 0);
    }

    // ── Non-move command processing ──────────────────────────────────────────

    #[test]
    fn test_non_move_command_charges_side_to_move() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        // After white's first move, side_to_move is black on the board.
        let board = make_test_board_state(PieceColor::Black);

        // Black resigns at t=1_300 (300s after white moved)
        process_non_move_command_timestamp_for_game_time(&mut s, &board, 1_300);

        assert_eq!(s.black_cumulative_seconds, 300);
        assert_eq!(s.white_cumulative_seconds, 0);
    }

    #[test]
    fn test_non_move_command_before_any_move_does_nothing() {
        let mut s = new_ten_minute_game_time();
        let board = make_test_board_state(PieceColor::White);
        process_non_move_command_timestamp_for_game_time(&mut s, &board, 1_000);

        assert_eq!(s.white_cumulative_seconds, 0);
        assert_eq!(s.black_cumulative_seconds, 0);
        assert_eq!(s.last_normal_move_unix_timestamp, None);
    }

    #[test]
    fn test_non_move_command_earlier_than_reference_charges_zero() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        let board = make_test_board_state(PieceColor::Black);
        process_non_move_command_timestamp_for_game_time(&mut s, &board, 500);

        assert_eq!(s.black_cumulative_seconds, 0);
    }

    #[test]
    fn test_non_move_command_can_flag() {
        let mut s = GameTimeState::new_initial_game_time_state(60);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        let board = make_test_board_state(PieceColor::Black);
        // Black takes 90s before resigning
        process_non_move_command_timestamp_for_game_time(&mut s, &board, 90);

        assert_eq!(s.timeflagged_player, Some(PieceColor::Black));
    }

    // ── Live flag check ──────────────────────────────────────────────────────

    #[test]
    fn test_live_flag_check_returns_none_when_time_remains() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        let board = make_test_board_state(PieceColor::Black);
        // 100s elapsed live; limit is 600
        assert_eq!(check_for_time_flag(&mut s, &board, 100), None);
        assert_eq!(s.timeflagged_player, None);
    }

    #[test]
    fn test_live_flag_check_sets_flag_when_exceeded() {
        let mut s = GameTimeState::new_initial_game_time_state(60);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        let board = make_test_board_state(PieceColor::Black);

        let result = check_for_time_flag(&mut s, &board, 61);
        assert_eq!(result, Some(PieceColor::Black));
        assert_eq!(s.timeflagged_player, Some(PieceColor::Black));
    }

    #[test]
    fn test_live_flag_check_does_not_advance_cumulative() {
        let mut s = GameTimeState::new_initial_game_time_state(60);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0);
        let board = make_test_board_state(PieceColor::Black);

        check_for_time_flag(&mut s, &board, 30);
        // Cumulative should still be zero; only a normal-move event commits.
        assert_eq!(s.black_cumulative_seconds, 0);
    }

    #[test]
    fn test_live_flag_check_before_first_move_returns_none() {
        let mut s = new_ten_minute_game_time();
        let board = make_test_board_state(PieceColor::White);
        assert_eq!(check_for_time_flag(&mut s, &board, 1_000), None);
        assert_eq!(s.timeflagged_player, None);
    }

    // ── Remaining-time query ────────────────────────────────────────────────

    #[test]
    fn test_remaining_full_before_game_start() {
        let s = new_ten_minute_game_time();
        let board = make_test_board_state(PieceColor::White);
        assert_eq!(
            compute_player_time_remaining_seconds(&s, &board, PieceColor::White, 1_000),
            600
        );
        assert_eq!(
            compute_player_time_remaining_seconds(&s, &board, PieceColor::Black, 1_000),
            600
        );
    }

    #[test]
    fn test_remaining_includes_live_elapsed_for_player_on_clock() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        let board = make_test_board_state(PieceColor::Black);

        let black_rem = compute_player_time_remaining_seconds(&s, &board, PieceColor::Black, 1_050);
        assert_eq!(black_rem, 600 - 50);

        let white_rem = compute_player_time_remaining_seconds(&s, &board, PieceColor::White, 1_050);
        assert_eq!(white_rem, 600);
    }

    #[test]
    fn test_remaining_floors_at_zero() {
        let mut s = GameTimeState::new_initial_game_time_state(60);
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        let board = make_test_board_state(PieceColor::Black);

        let rem = compute_player_time_remaining_seconds(&s, &board, PieceColor::Black, 10_000);
        assert_eq!(rem, 0);
    }

    #[test]
    fn test_remaining_after_mix_of_moves() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 0); // first
        process_move_timestamp_for_game_time(&mut s, PieceColor::Black, 30); // black used 30
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 50); // white used 20
        // Now Black is on the clock again
        let board = make_test_board_state(PieceColor::Black);

        let black_rem = compute_player_time_remaining_seconds(&s, &board, PieceColor::Black, 80);
        assert_eq!(black_rem, 600 - 30 - 30); // 30 stored + 30 live

        let white_rem = compute_player_time_remaining_seconds(&s, &board, PieceColor::White, 80);
        assert_eq!(white_rem, 600 - 20);
    }

    // ── Total elapsed query ─────────────────────────────────────────────────

    #[test]
    fn test_total_elapsed_zero_before_game_starts() {
        let s = new_ten_minute_game_time();
        assert_eq!(compute_total_elapsed_seconds_since_game_start(&s, 9_999), 0);
    }

    #[test]
    fn test_total_elapsed_since_game_start() {
        let mut s = new_ten_minute_game_time();
        process_move_timestamp_for_game_time(&mut s, PieceColor::White, 1_000);
        assert_eq!(
            compute_total_elapsed_seconds_since_game_start(&s, 1_300),
            300
        );
    }

    // ── HMS formatting ──────────────────────────────────────────────────────

    #[test]
    fn test_format_zero() {
        let mut buf = [0u8; 8];
        let n = format_seconds_as_hms_into_buffer(0, &mut buf).expect("ok");
        assert_eq!(&buf[..n], b"0:00:00");
    }

    #[test]
    fn test_format_one_second() {
        let mut buf = [0u8; 8];
        let n = format_seconds_as_hms_into_buffer(1, &mut buf).expect("ok");
        assert_eq!(&buf[..n], b"0:00:01");
    }

    #[test]
    fn test_format_seventy_five() {
        let mut buf = [0u8; 8];
        let n = format_seconds_as_hms_into_buffer(75, &mut buf).expect("ok");
        assert_eq!(&buf[..n], b"0:01:15");
    }

    #[test]
    fn test_format_ten_minutes() {
        let mut buf = [0u8; 8];
        let n = format_seconds_as_hms_into_buffer(600, &mut buf).expect("ok");
        assert_eq!(&buf[..n], b"0:10:00");
    }

    #[test]
    fn test_format_one_hour_two_three() {
        let mut buf = [0u8; 8];
        let n = format_seconds_as_hms_into_buffer(3_723, &mut buf).expect("ok");
        assert_eq!(&buf[..n], b"1:02:03");
    }

    #[test]
    fn test_format_ten_hours_needs_eight_bytes() {
        let mut buf = [0u8; 8];
        let n = format_seconds_as_hms_into_buffer(36_000, &mut buf).expect("ok");
        assert_eq!(&buf[..n], b"10:00:00");
    }

    #[test]
    fn test_format_buffer_too_small_errors() {
        let mut buf = [0u8; 6];
        let result = format_seconds_as_hms_into_buffer(75, &mut buf);
        assert_eq!(result, Err(MoveValidationError::InternalIndexOutOfBounds));
    }

    #[test]
    fn test_format_ten_hours_into_seven_byte_buffer_errors() {
        let mut buf = [0u8; 7];
        let result = format_seconds_as_hms_into_buffer(36_000, &mut buf);
        assert_eq!(result, Err(MoveValidationError::InternalIndexOutOfBounds));
    }
}

// ============================================================================
// SECTION 28: Memochess Game Config — Constants
// ============================================================================

/// Maximum bytes of the absolute directory path that holds the memo TOML
/// files for one memo_chess game.
///
/// ## Sizing Rationale
///
/// POSIX `PATH_MAX` is commonly 4096 on Linux, but realistic game-directory
/// paths in this project are well under 256 bytes. We accept 256 as a
/// hard upper bound for MVP-1. A user whose chosen path exceeds this limit
/// will receive an explicit configuration error rather than a silent
/// truncation.
///
/// If this limit proves too tight in practice, it may be raised in a
/// future revision. Raising it is a non-breaking change because all
/// callers reference the constant rather than hard-coding 256.
pub const MAX_DIRECTORY_PATH_BYTES: usize = 256;

/// Maximum bytes of a player or local-user name.
///
/// ## Sizing Rationale
///
/// 16 bytes is sufficient for short identifiers used in the TOML `owner`
/// field. This intentionally favors brevity. Names longer than 16 bytes
/// are rejected at configuration time with an explicit error so that no
/// later layer ever sees a truncated name.
pub const MAX_USERNAME_BYTES: usize = 16;

/// Lowest accepted value for `refresh_rate_seconds` in `MemochessGameConfig`.
///
/// A refresh rate of zero would cause a tight loop with no waiting; we
/// reject it.
pub const MIN_REFRESH_RATE_SECONDS: u8 = 1;

/// Highest accepted value for `refresh_rate_seconds` in `MemochessGameConfig`.
///
/// 240 seconds (4 minutes) is comfortably above any sensible refresh rate
/// for an interactive TUI; it gives a sanity-check ceiling without
/// constraining real use. The full `u8` range (up to 255) is also fine;
/// 240 is chosen to leave a small margin for any later "documented sentinel
/// values" without breaking compatibility.
pub const MAX_REFRESH_RATE_SECONDS: u8 = 240;

/// Lowest accepted value for `max_time_limit_per_player_seconds`.
///
/// One second is the floor. A zero per-player time budget would mean the
/// player flags before their first move, which is not a meaningful game.
pub const MIN_TIME_LIMIT_PER_PLAYER_SECONDS: u32 = 1;

/// Lowest accepted value of the N-move-rule, when enabled.
///
/// A "1-move rule" is not meaningful. 10 is a defensible floor that
/// admits all realistic N-move-rule choices (50 and 75 being the common
/// values).
pub const MIN_N_MOVE_RULE_VALUE: u16 = 10;

/// Highest accepted value of the N-move-rule, when enabled.
///
/// A 1000-move rule is well above any practical setting. This ceiling
/// exists only as a sanity check against malformed configuration values.
pub const MAX_N_MOVE_RULE_VALUE: u16 = 1000;

// ============================================================================
// SECTION 29: Memochess Game Config — Error Type
// ============================================================================

/// All possible failure modes when constructing or validating a
/// `MemochessGameConfig`.
///
/// ## Design Note
///
/// Like `MoveValidationError`, every variant is a unit variant. The enum
/// carries no data so that error values produced here cannot leak user
/// input, file paths, or other diagnostic content into production logs.
/// Callers that want to log a failure use `{:?}` formatting on the variant
/// at the layer where logging policy lives — not inside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemochessGameConfigError {
    /// The supplied directory-path byte slice was longer than
    /// `MAX_DIRECTORY_PATH_BYTES`.
    DirectoryPathTooLong,
    /// The supplied directory-path byte slice was empty.
    DirectoryPathEmpty,
    /// The supplied local-user-name byte slice was longer than
    /// `MAX_USERNAME_BYTES`.
    LocalUserNameTooLong,
    /// The supplied local-user-name byte slice was empty.
    LocalUserNameEmpty,
    /// The supplied white-player-name byte slice was longer than
    /// `MAX_USERNAME_BYTES`.
    WhitePlayerNameTooLong,
    /// The supplied white-player-name byte slice was empty.
    WhitePlayerNameEmpty,
    /// The supplied black-player-name byte slice was longer than
    /// `MAX_USERNAME_BYTES`.
    BlackPlayerNameTooLong,
    /// The supplied black-player-name byte slice was empty.
    BlackPlayerNameEmpty,
    /// The supplied per-player time limit was below
    /// `MIN_TIME_LIMIT_PER_PLAYER_SECONDS`.
    TimeLimitPerPlayerBelowMinimum,
    /// The supplied refresh rate was outside the closed interval
    /// [`MIN_REFRESH_RATE_SECONDS`, `MAX_REFRESH_RATE_SECONDS`].
    RefreshRateOutOfRange,
    /// The supplied N-move-rule value (when `Some`) was outside the
    /// closed interval [`MIN_N_MOVE_RULE_VALUE`, `MAX_N_MOVE_RULE_VALUE`].
    NMoveRuleOutOfRange,
    /// The supplied white and black player names were byte-identical.
    /// A game cannot be played with a single player on both sides via
    /// this configuration mechanism.
    WhiteAndBlackPlayerNamesIdentical,
}

// ============================================================================
// SECTION 30: Memochess Game Config — Struct
// ============================================================================

/// Configuration for one memo_chess game.
///
/// ## Project Context
///
/// This struct is the contract between the bootstrap layer
/// (`q_and_a_setup_bootstrap`, to be implemented) and the game-loop layer
/// (`DungeonMasterState`, to be implemented). The bootstrap layer
/// constructs and returns a fully-validated `MemochessGameConfig`; the
/// game-loop layer consumes it as input and never modifies it.
///
/// Two of these values cannot be supplied via the TOML memo files
/// themselves because they bootstrap the TOML-reading process:
///
/// - `directory_path` — where the memo files live.
/// - `local_user_name` — the identity of the user running this TUI
///   instance (which may or may not be one of the players; spectators
///   are supported).
///
/// All other fields are sourced from TOML memo files written by users
/// during bootstrap and parsed by `q_and_a_setup_bootstrap`.
///
/// ## Storage Strategy
///
/// All strings are stored as fixed-size byte arrays paired with a
/// `u8` length field (since both `MAX_DIRECTORY_PATH_BYTES` ≤ 256 and
/// `MAX_USERNAME_BYTES` ≤ 16 fit in a `u8`-representable length, and
/// we use `u16` for the directory length to leave headroom).
///
/// This pattern is consistent with the no-heap policy of the project.
/// `MemochessGameConfig` is `Copy`, so it can be passed by value to any
/// consumer without heap touches.
///
/// ## Bytes vs. UTF-8
///
/// Path and name bytes are stored as raw `u8` and are NOT validated as
/// UTF-8 at construction time. This is deliberate: filesystem paths are
/// byte strings on POSIX, and player names appear in user-controlled
/// TOML files. Layers that need to display these as text are responsible
/// for their own UTF-8 validation at the display boundary.
///
/// ## Threefold-Repetition Fields
///
/// The threefold-repetition rule fields are intentionally omitted from
/// MVP-1 (see the kept-but-commented lines in the struct body below).
/// The discussion in the project notes establishes that hash-based
/// threefold repetition is feasible but is deferred to a later
/// milestone. The commented lines remain as documentation of the future
/// shape; they are not active code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemochessGameConfig {
    /// Absolute path to the directory holding memo TOML files for this
    /// game. Bytes occupy `directory_path_buffer[..directory_path_length]`.
    pub directory_path_buffer: [u8; MAX_DIRECTORY_PATH_BYTES],
    /// Number of meaningful bytes in `directory_path_buffer`.
    /// In range `1..=MAX_DIRECTORY_PATH_BYTES`.
    pub directory_path_length: u16,

    /// Name of the user running this TUI instance.
    /// Bytes occupy `local_user_name_buffer[..local_user_name_length]`.
    pub local_user_name_buffer: [u8; MAX_USERNAME_BYTES],
    /// Number of meaningful bytes in `local_user_name_buffer`.
    /// In range `1..=MAX_USERNAME_BYTES`.
    pub local_user_name_length: u8,

    /// Name of the player playing White.
    pub white_player_name_buffer: [u8; MAX_USERNAME_BYTES],
    /// Number of meaningful bytes in `white_player_name_buffer`.
    pub white_player_name_length: u8,

    /// Name of the player playing Black.
    pub black_player_name_buffer: [u8; MAX_USERNAME_BYTES],
    /// Number of meaningful bytes in `black_player_name_buffer`.
    pub black_player_name_length: u8,

    /// Per-player thinking-time budget in seconds. When either player's
    /// cumulative thinking time reaches this value, that player flags.
    pub max_time_limit_per_player_seconds: u32,

    /// Refresh cadence of the game loop, in seconds. The loop wakes,
    /// scans for new files, updates state, and renders once per
    /// `refresh_rate_seconds`.
    pub refresh_rate_seconds: u8,

    /// Optional automatic-draw rule: if `Some(n)`, the game is drawn
    /// after `n` consecutive half-moves with no pawn move and no
    /// capture. `None` disables the rule. Common settings are 50 or 75.
    pub n_move_rule: Option<u16>,
    // -------------------------------------------------------------------
    // MVP-1: threefold-repetition is intentionally out of scope.
    // The shape below documents the future fields without enabling them.
    //
    // /// If true, the game enforces a soft threefold-repetition rule
    // /// (game is drawn when a position recurs three times). Implementation
    // /// will require hashed position history.
    // pub three_time_rep_rule: bool,
    //
    // /// If true, the game enforces a hard threefold-repetition rule
    // /// (automatic draw on detection, no negotiation). Implementation
    // /// will require hashed position history.
    // pub hard_3time_rep_rule: bool,
    // -------------------------------------------------------------------
}

// ============================================================================
// SECTION 31: Memochess Game Config — Construction
// ============================================================================

impl MemochessGameConfig {
    /// Construct a `MemochessGameConfig` from validated inputs.
    ///
    /// ## Project Context
    ///
    /// Called by `q_and_a_setup_bootstrap` once all required configuration
    /// values have been collected. May also be called by a test or by
    /// `main.rs` of a stand-alone demo with hard-coded values.
    ///
    /// All inputs are validated; any failure produces a unit-variant
    /// `MemochessGameConfigError`. On success, the returned struct is
    /// guaranteed to satisfy the invariants documented on each field.
    ///
    /// ## Arguments
    ///
    /// - `directory_path_bytes`: absolute path bytes. Must be non-empty
    ///   and no longer than `MAX_DIRECTORY_PATH_BYTES`.
    /// - `local_user_name_bytes`: local user name. Must be non-empty
    ///   and no longer than `MAX_USERNAME_BYTES`.
    /// - `white_player_name_bytes`: White player name. Must be non-empty
    ///   and no longer than `MAX_USERNAME_BYTES`.
    /// - `black_player_name_bytes`: Black player name. Must be non-empty,
    ///   no longer than `MAX_USERNAME_BYTES`, and not byte-equal to
    ///   the white name.
    /// - `max_time_limit_per_player_seconds`: per-player time budget.
    ///   Must be at least `MIN_TIME_LIMIT_PER_PLAYER_SECONDS`.
    /// - `refresh_rate_seconds`: game-loop tick interval. Must lie in
    ///   `[MIN_REFRESH_RATE_SECONDS, MAX_REFRESH_RATE_SECONDS]`.
    /// - `n_move_rule`: optional N-move rule. If `Some(n)`, `n` must
    ///   lie in `[MIN_N_MOVE_RULE_VALUE, MAX_N_MOVE_RULE_VALUE]`.
    ///
    /// ## Failure Modes
    ///
    /// Returns `Err` for any individual field that fails its bound check.
    /// Validations are performed in field order; only the first
    /// detected failure is reported.
    ///
    /// ## Memory & Panic Policy
    ///
    /// No heap. No panics. All buffer writes are bounds-checked
    /// by `copy_bytes_into_fixed_buffer`.
    pub fn try_construct_memochess_game_config(
        directory_path_bytes: &[u8],
        local_user_name_bytes: &[u8],
        white_player_name_bytes: &[u8],
        black_player_name_bytes: &[u8],
        max_time_limit_per_player_seconds: u32,
        refresh_rate_seconds: u8,
        n_move_rule: Option<u16>,
    ) -> Result<MemochessGameConfig, MemochessGameConfigError> {
        // ── Directory path ────────────────────────────────────────────
        if directory_path_bytes.is_empty() {
            return Err(MemochessGameConfigError::DirectoryPathEmpty);
        }
        if directory_path_bytes.len() > MAX_DIRECTORY_PATH_BYTES {
            return Err(MemochessGameConfigError::DirectoryPathTooLong);
        }

        // ── Local user name ───────────────────────────────────────────
        if local_user_name_bytes.is_empty() {
            return Err(MemochessGameConfigError::LocalUserNameEmpty);
        }
        if local_user_name_bytes.len() > MAX_USERNAME_BYTES {
            return Err(MemochessGameConfigError::LocalUserNameTooLong);
        }

        // ── White player name ─────────────────────────────────────────
        if white_player_name_bytes.is_empty() {
            return Err(MemochessGameConfigError::WhitePlayerNameEmpty);
        }
        if white_player_name_bytes.len() > MAX_USERNAME_BYTES {
            return Err(MemochessGameConfigError::WhitePlayerNameTooLong);
        }

        // ── Black player name ─────────────────────────────────────────
        if black_player_name_bytes.is_empty() {
            return Err(MemochessGameConfigError::BlackPlayerNameEmpty);
        }
        if black_player_name_bytes.len() > MAX_USERNAME_BYTES {
            return Err(MemochessGameConfigError::BlackPlayerNameTooLong);
        }

        // ── Distinct white and black names ────────────────────────────
        if white_player_name_bytes == black_player_name_bytes {
            return Err(MemochessGameConfigError::WhiteAndBlackPlayerNamesIdentical);
        }

        // ── Time limit ────────────────────────────────────────────────
        if max_time_limit_per_player_seconds < MIN_TIME_LIMIT_PER_PLAYER_SECONDS {
            return Err(MemochessGameConfigError::TimeLimitPerPlayerBelowMinimum);
        }

        // ── Refresh rate ──────────────────────────────────────────────
        if refresh_rate_seconds < MIN_REFRESH_RATE_SECONDS
            || refresh_rate_seconds > MAX_REFRESH_RATE_SECONDS
        {
            return Err(MemochessGameConfigError::RefreshRateOutOfRange);
        }

        // ── N-move rule (when present) ────────────────────────────────
        if let Some(n_value) = n_move_rule {
            if n_value < MIN_N_MOVE_RULE_VALUE || n_value > MAX_N_MOVE_RULE_VALUE {
                return Err(MemochessGameConfigError::NMoveRuleOutOfRange);
            }
        }

        // ── All checks passed; populate fixed-size buffers ────────────
        let mut directory_path_buffer = [0u8; MAX_DIRECTORY_PATH_BYTES];
        let directory_path_length =
            copy_bytes_into_fixed_buffer(directory_path_bytes, &mut directory_path_buffer)?;

        let mut local_user_name_buffer = [0u8; MAX_USERNAME_BYTES];
        let local_user_name_length =
            copy_bytes_into_fixed_buffer(local_user_name_bytes, &mut local_user_name_buffer)?;

        let mut white_player_name_buffer = [0u8; MAX_USERNAME_BYTES];
        let white_player_name_length =
            copy_bytes_into_fixed_buffer(white_player_name_bytes, &mut white_player_name_buffer)?;

        let mut black_player_name_buffer = [0u8; MAX_USERNAME_BYTES];
        let black_player_name_length =
            copy_bytes_into_fixed_buffer(black_player_name_bytes, &mut black_player_name_buffer)?;

        // Defensive narrowing: the length checks above guarantee these
        // fit in their target types, but we re-check via debug_assert
        // and prod-safe handling to make the narrowing explicit.
        let directory_path_length_u16: u16 = if directory_path_length <= MAX_DIRECTORY_PATH_BYTES {
            directory_path_length as u16
        } else {
            // Unreachable in practice: bounds were checked above.
            return Err(MemochessGameConfigError::DirectoryPathTooLong);
        };

        let local_user_name_length_u8: u8 = if local_user_name_length <= MAX_USERNAME_BYTES {
            local_user_name_length as u8
        } else {
            return Err(MemochessGameConfigError::LocalUserNameTooLong);
        };

        let white_player_name_length_u8: u8 = if white_player_name_length <= MAX_USERNAME_BYTES {
            white_player_name_length as u8
        } else {
            return Err(MemochessGameConfigError::WhitePlayerNameTooLong);
        };

        let black_player_name_length_u8: u8 = if black_player_name_length <= MAX_USERNAME_BYTES {
            black_player_name_length as u8
        } else {
            return Err(MemochessGameConfigError::BlackPlayerNameTooLong);
        };

        Ok(MemochessGameConfig {
            directory_path_buffer,
            directory_path_length: directory_path_length_u16,
            local_user_name_buffer,
            local_user_name_length: local_user_name_length_u8,
            white_player_name_buffer,
            white_player_name_length: white_player_name_length_u8,
            black_player_name_buffer,
            black_player_name_length: black_player_name_length_u8,
            max_time_limit_per_player_seconds,
            refresh_rate_seconds,
            n_move_rule,
        })
    }

    /// Borrow the directory-path bytes as a slice.
    ///
    /// The returned slice references only the meaningful prefix
    /// (`..directory_path_length`). It is not null-terminated.
    pub fn directory_path_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.directory_path_length as usize;
        // Defensive clamp: in case length ever exceeded buffer size
        // (it cannot via the public API), avoid panicking on slice.
        let safe_length = if length_as_usize > MAX_DIRECTORY_PATH_BYTES {
            MAX_DIRECTORY_PATH_BYTES
        } else {
            length_as_usize
        };
        &self.directory_path_buffer[..safe_length]
    }

    /// Borrow the local-user-name bytes as a slice.
    pub fn local_user_name_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.local_user_name_length as usize;
        let safe_length = if length_as_usize > MAX_USERNAME_BYTES {
            MAX_USERNAME_BYTES
        } else {
            length_as_usize
        };
        &self.local_user_name_buffer[..safe_length]
    }

    /// Borrow the white-player-name bytes as a slice.
    pub fn white_player_name_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.white_player_name_length as usize;
        let safe_length = if length_as_usize > MAX_USERNAME_BYTES {
            MAX_USERNAME_BYTES
        } else {
            length_as_usize
        };
        &self.white_player_name_buffer[..safe_length]
    }

    /// Borrow the black-player-name bytes as a slice.
    pub fn black_player_name_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.black_player_name_length as usize;
        let safe_length = if length_as_usize > MAX_USERNAME_BYTES {
            MAX_USERNAME_BYTES
        } else {
            length_as_usize
        };
        &self.black_player_name_buffer[..safe_length]
    }
}

// ============================================================================
// SECTION 32: Memochess Game Config — Internal Helper
// ============================================================================

/// Copy `source_bytes` into the start of `destination_buffer` and return
/// the number of bytes copied.
///
/// Returns `Err(MemochessGameConfigError::DirectoryPathTooLong)` if
/// `source_bytes` does not fit. The specific error variant used here is
/// deliberately one of the "too long" variants; the caller is expected to
/// have already performed a more specific length check against the
/// appropriate maximum constant *before* calling this helper, so this
/// error path is a defensive backstop only.
///
/// Internal helper. No heap, no panics.
fn copy_bytes_into_fixed_buffer(
    source_bytes: &[u8],
    destination_buffer: &mut [u8],
) -> Result<usize, MemochessGameConfigError> {
    if source_bytes.len() > destination_buffer.len() {
        // Defensive backstop only; the caller-side length checks should
        // make this unreachable. We surface a generic "too long"
        // variant here. (The caller has already returned its own more
        // specific variant before reaching this helper.)
        return Err(MemochessGameConfigError::DirectoryPathTooLong);
    }
    destination_buffer[..source_bytes.len()].copy_from_slice(source_bytes);
    Ok(source_bytes.len())
}

// ============================================================================
// SECTION 33: Cargo Tests for MemochessGameConfig
// ============================================================================

#[cfg(test)]
mod tests_memochess_game_config {
    use super::*;

    /// Helper: construct a valid config for tests, returning the result.
    fn build_valid_test_config() -> Result<MemochessGameConfig, MemochessGameConfigError> {
        MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/memo_chess_demo",
            b"tom",
            b"alice",
            b"bob",
            600,      // 10-minute game
            10,       // refresh every 10 seconds
            Some(50), // 50-move rule enabled
        )
    }

    #[test]
    fn valid_config_constructs_successfully() {
        let config = build_valid_test_config().expect("test: valid inputs must construct");
        assert_eq!(config.directory_path_as_bytes(), b"/tmp/memo_chess_demo");
        assert_eq!(config.local_user_name_as_bytes(), b"tom");
        assert_eq!(config.white_player_name_as_bytes(), b"alice");
        assert_eq!(config.black_player_name_as_bytes(), b"bob");
        assert_eq!(config.max_time_limit_per_player_seconds, 600);
        assert_eq!(config.refresh_rate_seconds, 10);
        assert_eq!(config.n_move_rule, Some(50));
    }

    #[test]
    fn config_with_no_n_move_rule_constructs_successfully() {
        let config = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/game",
            b"u",
            b"w",
            b"b",
            60,
            5,
            None,
        )
        .expect("test: None n_move_rule must be accepted");
        assert_eq!(config.n_move_rule, None);
    }

    #[test]
    fn empty_directory_path_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"", b"u", b"w", b"b", 60, 5, None,
        );
        assert_eq!(result, Err(MemochessGameConfigError::DirectoryPathEmpty));
    }

    #[test]
    fn oversize_directory_path_rejected() {
        let oversize_path = [b'a'; MAX_DIRECTORY_PATH_BYTES + 1];
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            &oversize_path,
            b"u",
            b"w",
            b"b",
            60,
            5,
            None,
        );
        assert_eq!(result, Err(MemochessGameConfigError::DirectoryPathTooLong));
    }

    #[test]
    fn maximum_length_directory_path_accepted() {
        let max_path = [b'a'; MAX_DIRECTORY_PATH_BYTES];
        let config = MemochessGameConfig::try_construct_memochess_game_config(
            &max_path, b"u", b"w", b"b", 60, 5, None,
        )
        .expect("test: exactly max-length path must be accepted");
        assert_eq!(
            config.directory_path_as_bytes().len(),
            MAX_DIRECTORY_PATH_BYTES
        );
    }

    #[test]
    fn empty_local_user_name_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"", b"w", b"b", 60, 5, None,
        );
        assert_eq!(result, Err(MemochessGameConfigError::LocalUserNameEmpty));
    }

    #[test]
    fn oversize_local_user_name_rejected() {
        let oversize_name = [b'x'; MAX_USERNAME_BYTES + 1];
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g",
            &oversize_name,
            b"w",
            b"b",
            60,
            5,
            None,
        );
        assert_eq!(result, Err(MemochessGameConfigError::LocalUserNameTooLong));
    }

    #[test]
    fn maximum_length_username_accepted() {
        let max_name = [b'a'; MAX_USERNAME_BYTES];
        let other_max_name = [b'b'; MAX_USERNAME_BYTES];
        let config = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g",
            b"u",
            &max_name,
            &other_max_name,
            60,
            5,
            None,
        )
        .expect("test: max-length names must be accepted");
        assert_eq!(config.white_player_name_as_bytes(), &max_name[..]);
        assert_eq!(config.black_player_name_as_bytes(), &other_max_name[..]);
    }

    #[test]
    fn identical_white_and_black_names_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"u", b"alice", b"alice", 60, 5, None,
        );
        assert_eq!(
            result,
            Err(MemochessGameConfigError::WhiteAndBlackPlayerNamesIdentical)
        );
    }

    #[test]
    fn zero_time_limit_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"u", b"w", b"b", 0, 5, None,
        );
        assert_eq!(
            result,
            Err(MemochessGameConfigError::TimeLimitPerPlayerBelowMinimum)
        );
    }

    #[test]
    fn refresh_rate_zero_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"u", b"w", b"b", 60, 0, None,
        );
        assert_eq!(result, Err(MemochessGameConfigError::RefreshRateOutOfRange));
    }

    #[test]
    fn refresh_rate_too_high_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g",
            b"u",
            b"w",
            b"b",
            60,
            MAX_REFRESH_RATE_SECONDS + 1,
            None,
        );
        assert_eq!(result, Err(MemochessGameConfigError::RefreshRateOutOfRange));
    }

    #[test]
    fn n_move_rule_too_low_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g",
            b"u",
            b"w",
            b"b",
            60,
            5,
            Some(MIN_N_MOVE_RULE_VALUE - 1),
        );
        assert_eq!(result, Err(MemochessGameConfigError::NMoveRuleOutOfRange));
    }

    #[test]
    fn n_move_rule_too_high_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g",
            b"u",
            b"w",
            b"b",
            60,
            5,
            Some(MAX_N_MOVE_RULE_VALUE + 1),
        );
        assert_eq!(result, Err(MemochessGameConfigError::NMoveRuleOutOfRange));
    }

    #[test]
    fn config_is_copy() {
        // Compile-time assertion that the struct really is Copy.
        fn assert_copy<T: Copy>() {}
        assert_copy::<MemochessGameConfig>();
        assert_copy::<MemochessGameConfigError>();
    }

    #[test]
    fn config_round_trips_through_copy() {
        let original = build_valid_test_config().expect("test: build");
        let copied = original; // would move if not Copy
        // Both must remain usable.
        assert_eq!(
            original.directory_path_as_bytes(),
            copied.directory_path_as_bytes()
        );
        assert_eq!(
            original.white_player_name_as_bytes(),
            copied.white_player_name_as_bytes()
        );
    }

    #[test]
    fn byte_slices_do_not_include_buffer_tail() {
        let config = build_valid_test_config().expect("test: build");
        // `tom` is 3 bytes; the buffer is 16 bytes. The slice must be
        // exactly 3 bytes, not 16.
        assert_eq!(config.local_user_name_as_bytes(), b"tom");
        assert_eq!(config.local_user_name_as_bytes().len(), 3);
    }
}

// ============================================================================
// read_toml_single_line_string_field_no_heap
// ============================================================================

// https://github.com/lineality/toml_read_field_noheap_rust

// # Project Context (Strategic Scope)

// Many production deployments need to read a handful of short configuration
// values (e.g. a node identifier, a mode flag, a short key fingerprint) from
// a TOML file at startup. The standard-library idiom
// `BufReader::new(File).lines()` is unsuitable for several production
// contexts because it:

//   * Heap-allocates a buffer (~8 KiB) inside `BufReader`.
//   * Heap-allocates a fresh `String` for every line via `lines()`.
//   * Returns owned `String` values, propagating heap use to the caller.

// Heap allocation in production hot paths or early-boot paths is undesirable
// because it: enlarges attack surface (allocator bugs, OOM-as-DoS), defeats
// static memory budgeting, complicates real-time guarantees, and obscures the
// true memory footprint of the program.

// This module exposes a single function,
// [`read_single_line_string_field_from_toml_no_heap`], that reads a single
// short-string field from a TOML file using only stack-allocated buffers.

// # In Scope

// * One key per call, top-level (no `[section]`).
// * Single-line values up to a caller-chosen `OUTPUT_BUFFER_BYTES` length.
// * Values quoted with simple double quotes (`"..."`) or unquoted (numbers,
//   bare identifiers).
// * Lines using LF or CRLF terminators.
// * Lines beginning with `#` (after trimming) are treated as comments.

// # Explicitly Out Of Scope (Non-Goals)

// * Full TOML grammar (no arrays, tables, inline tables, multi-line strings,
//   escape sequences, dotted keys, datetimes).
// * Re-encoding the value (caller decides whether to `core::str::from_utf8`).
// * Trailing inline comments on the same line as the value
//   (e.g. `name = "x"  # note` — the `# note` becomes part of the value).
//   If your project relies on inline comments, switch to a real parser.
// * UTF-8 BOM at file start (the BOM bytes will appear in the first line's
//   key prefix and cause a non-match for that key). Strip BOMs upstream if
//   your toolchain may produce them.

// # Defensive Policy

// On any malformed input, I/O error, oversize line, oversize value, or
// exhausted safety budget, the function returns a terse zero-data
// [`ReadTomlFieldError`] variant. It never panics, never allocates, and
// never includes the file path, file contents, or OS error string in the
// returned error. Callers that need richer diagnostics should add them
// in `#[cfg(debug_assertions)]`-gated code, not in production.

// # Concurrency

// The function is synchronous and self-contained. It does not share state.
// It is safe to call from multiple threads with distinct paths.
// ============================================================================

/*
Example use, main.rs code

//! ============================================================================
//! Binary: rslsf_demo
//! ----------------------------------------------------------------------------
//! Demonstrates and smoke-tests `read_single_line_string_field_from_toml_no_heap`
//! against a real on-disk TOML file.
//!
//! # Project Context
//! This binary is intentionally minimal. Its job is to:
//!   1. Write a small known TOML file to a deterministic absolute path.
//!   2. Read a single short field from it using the no-heap reader.
//!   3. Print a terse, fixed-set status — no path, no value contents in error
//!      branches — matching the production logging policy of the module.
//!
//! Note: this binary uses `println!` for the *success* path only as a
//! human-facing demonstration. Real production callers should route output
//! through their own logging facility.
//! ============================================================================

/*

## 16 is not hardcoded anywhere in the module.

The `16` lives only in `main.rs`:

```rust
const DEMO_OUTPUT_BUFFER_BYTES: usize = 16;   // demo's choice, not the module's
```

and is passed to the function as a **const generic** at the call site:

```rust
read_single_line_string_field_from_toml_no_heap::<DEMO_OUTPUT_BUFFER_BYTES>(...)
//                                                ^^^^^^^^^^^^^^^^^^^^^^^^
//                                                this is the knob
```

You can call it with any compile-time size you like:

```rust
read_single_line_string_field_from_toml_no_heap::<8>(path, "x")?;
read_single_line_string_field_from_toml_no_heap::<32>(path, "x")?;
read_single_line_string_field_from_toml_no_heap::<256>(path, "x")?;
```

Each call site picks its own value buffer size. They do not interfere with each other.

## There are actually three buffers in play. Only one is caller-tunable today.

| Buffer | Purpose | Size today | Where set | Caller-tunable? |
|---|---|---|---|---|
| **Output value buffer** | Holds the extracted field value | `OUTPUT_BUFFER_BYTES` (e.g. `16`) | const generic at call site | **Yes** |
| **File read chunk** | One `file.read()` lands here | `RSLSF_READ_CHUNK_BYTES = 256` | module constant | No (today) |
| **Line accumulator** | Reassembles a single line across chunks | `RSLSF_MAX_LINE_BYTES = 512` | module constant | No (today) |

The two internal buffers (256 and 512) are fixed for every caller. The output buffer is per-call.

## Why the asymmetry, and how to change it if you want symmetry

The asymmetry exists because, in practice:

- Callers care a lot about the **output size** — it determines what value lengths they accept, and they want it as small as their data allows (no wasted stack).
- Callers usually do **not** care about the internal scan buffer sizes — those just need to be "big enough for any reasonable line."

If your project does want all three tunable (e.g. you are on a microcontroller with 8 KiB of stack and want to shrink the line accumulator to 128 bytes), promote them to const generics as well:

```rust
pub fn read_single_line_string_field_from_toml_no_heap<
    const OUTPUT_BUFFER_BYTES: usize,
    const READ_CHUNK_BYTES:    usize,
    const MAX_LINE_BYTES:      usize,
>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> { ... }
```

Call site then becomes:

```rust
read_single_line_string_field_from_toml_no_heap::<16, 256, 512>(path, "name")?;
```

That trade is verbosity at call sites for full caller control of stack footprint. Pick whichever side you want; both are stack-only and heap-free.

*/

mod read_toml_single_line_string_field_no_heap;

use read_toml_single_line_string_field_no_heap::{
    ReadTomlFieldError, read_single_line_string_field_from_toml_no_heap,
};

use std::io::Write;

/// Size of the on-stack output buffer used in this demo.
/// Picked to comfortably hold the demo value `"alice-node-01"` (13 bytes)
/// without being wastefully large.
const DEMO_OUTPUT_BUFFER_BYTES: usize = 16;

/// Name of the demo TOML file written under the OS temp directory.
const DEMO_TOML_FILE_NAME: &str = "rslsf_demo_config.toml";

/// Contents of the demo TOML file. Kept simple and inside the in-scope
/// subset documented by the reader module (no sections, no escapes,
/// no multi-line strings).
const DEMO_TOML_FILE_CONTENTS: &str = "\
# rslsf_demo: sample configuration
# This file is overwritten on every run.

node_id   = \"alice-node-01\"
mode      = \"production\"
port      = 8080
";

/// Process exit codes are kept few and fixed to avoid leaking detail.
/// Production policy: never propagate raw OS errors to the exit code.
const EXIT_OK: i32 = 0;
const EXIT_DEMO_SETUP_FAILED: i32 = 10;
const EXIT_READ_FAILED: i32 = 20;

fn main() {
    // ------------------------------------------------------------------
    // Step 1: write the demo TOML file to an absolute path under temp_dir.
    // This step is the demo's responsibility — it is NOT a production
    // pattern, just a way to give the reader something to read.
    // ------------------------------------------------------------------
    let mut demo_file_absolute_path = std::env::temp_dir();
    demo_file_absolute_path.push(DEMO_TOML_FILE_NAME);

    match std::fs::File::create(&demo_file_absolute_path)
        .and_then(|mut f| f.write_all(DEMO_TOML_FILE_CONTENTS.as_bytes()))
    {
        Ok(()) => {}
        Err(_) => {
            // Terse message: do NOT print the path or OS error.
            eprintln!("rslsf_demo: setup failed");
            std::process::exit(EXIT_DEMO_SETUP_FAILED);
        }
    }

    // Convert PathBuf to &str for the reader (which takes &str).
    // If the temp dir has a non-UTF-8 path on this platform, we bail
    // cleanly without exposing the path.
    let demo_file_path_as_str: &str = match demo_file_absolute_path.to_str() {
        Some(s) => s,
        None => {
            eprintln!("rslsf_demo: setup failed");
            std::process::exit(EXIT_DEMO_SETUP_FAILED);
        }
    };

    // ------------------------------------------------------------------
    // Step 2: read three fields and print results in a fixed-format way.
    // We deliberately call the reader three times to demonstrate that
    // each call is independent and reuses no state.
    // ------------------------------------------------------------------
    print_field_or_terse_error("node_id", demo_file_path_as_str);
    print_field_or_terse_error("mode", demo_file_path_as_str);
    print_field_or_terse_error("port", demo_file_path_as_str);
    print_field_or_terse_error("missing_key", demo_file_path_as_str);

    std::process::exit(EXIT_OK);
}

/// Read one field and print a single fixed-format line.
///
/// On success the line is `OK <key> = <value>`.
/// On any error the line is `ERR <key> <error-code>` with NO path,
/// NO file contents, NO OS error text — matching the module's policy.
fn print_field_or_terse_error(target_field_key: &str, absolute_toml_file_path: &str) {
    let read_result = read_single_line_string_field_from_toml_no_heap::<DEMO_OUTPUT_BUFFER_BYTES>(
        absolute_toml_file_path,
        target_field_key,
    );

    match read_result {
        Ok((output_buffer, written_length)) => {
            // Validate UTF-8 at the boundary, since we want to print as text.
            // This validation is the caller's responsibility, not the reader's.
            match core::str::from_utf8(&output_buffer[..written_length]) {
                Ok(value_as_str) => {
                    println!("OK  {} = {}", target_field_key, value_as_str);
                }
                Err(_) => {
                    // Value is not UTF-8: report terse, do not print bytes.
                    println!("ERR {} non_utf8_value", target_field_key);
                }
            }
        }
        Err(error_variant) => {
            println!(
                "ERR {} {}",
                target_field_key,
                terse_error_code(error_variant),
            );

            // Demo policy: if the failure is something other than
            // "field not found" we treat it as a hard failure and
            // exit. In real production code, the caller would handle
            // each variant according to its own recovery policy and
            // would NOT terminate the program.
            match error_variant {
                ReadTomlFieldError::RsLsfFieldNotFound => {
                    // expected for "missing_key"; keep going.
                }
                _ => {
                    std::process::exit(EXIT_READ_FAILED);
                }
            }
        }
    }
}

/// Map each error variant to a short, fixed, log-safe code.
///
/// These codes are stable strings the operator can grep for. They contain
/// no path, no contents, and no OS detail — by design.
fn terse_error_code(error_variant: ReadTomlFieldError) -> &'static str {
    match error_variant {
        ReadTomlFieldError::RsLsfEmptyKey => "E_EMPTY_KEY",
        ReadTomlFieldError::RsLsfKeyTooLong => "E_KEY_TOO_LONG",
        ReadTomlFieldError::RsLsfOutputBufferZeroSized => "E_OUTBUF_ZERO",
        ReadTomlFieldError::RsLsfFileOpenFailed => "E_OPEN",
        ReadTomlFieldError::RsLsfFileReadFailed => "E_READ",
        ReadTomlFieldError::RsLsfFieldNotFound => "E_NOT_FOUND",
        ReadTomlFieldError::RsLsfValueExceedsOutputBuffer => "E_VALUE_TOO_BIG",
        ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer => "E_LINE_TOO_BIG",
        ReadTomlFieldError::RsLsfSafetyBudgetExhausted => "E_SAFETY_BUDGET",
    }
}



*/

use std::fs::File;
use std::io::Read;

// ----------------------------------------------------------------------------
// Module constants
// ----------------------------------------------------------------------------

/// Stack-allocated chunk size used for `File::read`.
///
/// Tradeoff: smaller chunks reduce stack pressure; larger chunks reduce syscall
/// count. 256 B is comfortable on every realistic stack and keeps syscall
/// overhead acceptable for the small-config use case this module targets.
const RSLSF_READ_CHUNK_BYTES: usize = 16;

/// Maximum bytes accumulated for a single line during scanning (stack-only).
///
/// Lines exceeding this limit do NOT silently truncate; see overflow handling
/// in [`read_single_line_string_field_from_toml_no_heap`]. 512 B comfortably
/// covers any realistic single-line TOML key/value in the in-scope subset.
pub const RSLSF_MAX_LINE_BYTES: usize = 64;

/// Failsafe upper bound on total bytes scanned from a single file.
///
/// Bounds the read loop even if the OS keeps returning data (NASA P10 rule 2).
/// Tune for your project's expected configuration size. 1 MiB is generous for
/// configuration files while preventing pathological/adversarial inputs from
/// running unbounded work.
pub const RSLSF_MAX_BYTES_SCANNED: u64 = 1 << 20;

// ----------------------------------------------------------------------------
// Error type
// ----------------------------------------------------------------------------

/// Production-safe error type for
/// [`read_single_line_string_field_from_toml_no_heap`].
///
/// # Design
///
/// * All variants are zero-sized: no heap, no `String`, no embedded path,
///   no embedded OS error. This is a deliberate defensive choice — error
///   values must never become an information-disclosure vector.
/// * Every variant carries the unique prefix `RsLsf` (Read Single Line
///   String Field) so it is unambiguously traceable in logs to this
///   function, satisfying the "unique error per function" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTomlFieldError {
    /// RSLSF: caller-supplied key was empty.
    RsLsfEmptyKey,
    /// RSLSF: caller-supplied key would not fit in the line scan buffer.
    RsLsfKeyTooLong,
    /// RSLSF: caller-supplied `OUTPUT_BUFFER_BYTES` const-generic was zero.
    RsLsfOutputBufferZeroSized,
    /// RSLSF: the file could not be opened (does not exist, permission, etc.).
    RsLsfFileOpenFailed,
    /// RSLSF: an I/O error occurred while reading.
    RsLsfFileReadFailed,
    /// RSLSF: the requested key was not present in the file.
    RsLsfFieldNotFound,
    /// RSLSF: the matched value would not fit in `OUTPUT_BUFFER_BYTES`.
    RsLsfValueExceedsOutputBuffer,
    /// RSLSF: a line whose leading bytes matched the requested key exceeded
    /// `RSLSF_MAX_LINE_BYTES`; refusing to silently truncate.
    RsLsfMatchingLineExceedsScanBuffer,
    /// RSLSF: the failsafe byte/iteration budget was exhausted.
    RsLsfSafetyBudgetExhausted,
}

// ----------------------------------------------------------------------------
// Public API
// ----------------------------------------------------------------------------

/// Reads a single top-level single-line string field from a TOML file using
/// only stack-allocated memory (buffer size set by call to function).
///
/// e.g.
/// read_single_line_string_field_from_toml_no_heap::<8>(path, "x")?;
/// read_single_line_string_field_from_toml_no_heap::<32>(path, "x")?;
/// read_single_line_string_field_from_toml_no_heap::<256>(path, "x")?;
///
/// # Type Parameters
/// * `OUTPUT_BUFFER_BYTES` — the fixed size of the returned byte buffer.
///   Must be `> 0`. Pick the smallest value that comfortably fits your
///   project's value (e.g. `16` for a short identifier).
///
/// # Arguments
/// * `absolute_toml_file_path` — absolute path to the TOML file. Relative
///   paths technically work but are discouraged per project policy because
///   they depend on the current working directory.
/// * `target_field_key` — the exact top-level key to find. Must be non-empty
///   and shorter than [`RSLSF_MAX_LINE_BYTES`].
///
/// # Returns
/// * `Ok((output_buffer, written_length))` on success. `output_buffer` is
///   `[u8; OUTPUT_BUFFER_BYTES]`, and `written_length` bytes of it are
///   meaningful. Bytes past `written_length` are zero.
/// * `Err(ReadTomlFieldError)` on any failure; never panics, never allocates.
///
/// # Example (illustrative)
/// ```ignore
/// match read_single_line_string_field_from_toml_no_heap::<16>(
///     "/etc/myapp/config.toml",
///     "node_id",
/// ) {
///     Ok((buf, len)) => {
///         // Caller decides whether to validate UTF-8.
///         if let Ok(s) = core::str::from_utf8(&buf[..len]) {
///             // use s
///         }
///     }
///     Err(_e) => {
///         // Production: log a unique short code, do NOT log path/contents.
///         // Continue with safe default; do not panic.
///     }
/// }
/// ```
/// # Type Parameters
/// * `OUTPUT_BUFFER_BYTES` — chosen by the caller at the call site, at
///   compile time, via the turbofish syntax `::<N>`. This is the size of
///   the value buffer returned to the caller; it is the maximum value
///   length this call will accept. There is no default and no hardcoded
///   value inside this module. Pick the smallest `N` that fits the
///   longest legitimate value your caller will accept; values longer
///   than `N` produce `RsLsfValueExceedsOutputBuffer` and are never
///   silently truncated.
///
/// # Internal buffers (not caller-tunable in this version)
/// Two internal scan buffers are sized by module-level constants:
/// [`RSLSF_READ_CHUNK_BYTES`] and [`RSLSF_MAX_LINE_BYTES`]. If your
/// project needs to tune those (e.g. tighter stack budget on embedded
/// targets), promote them to additional const generics on this function.
///
fn read_single_line_string_field_from_toml_no_heap<const OUTPUT_BUFFER_BYTES: usize>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> {
    // ----------------------------------------------------------------------
    // Debug-Assert / Test-Assert / Production-Catch
    // ----------------------------------------------------------------------
    // Debug-only assertion: panics only in non-test debug builds, so
    // developers see violated preconditions during local iteration.
    #[cfg(all(debug_assertions, not(test)))]
    {
        debug_assert!(
            OUTPUT_BUFFER_BYTES > 0,
            "RSLSF: OUTPUT_BUFFER_BYTES must be > 0",
        );
        debug_assert!(
            !target_field_key.is_empty(),
            "RSLSF: target_field_key must not be empty",
        );
    }

    // Production catch-handles: never panic; convert violations into errors.
    if OUTPUT_BUFFER_BYTES == 0 {
        return Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized);
    }
    if target_field_key.is_empty() {
        return Err(ReadTomlFieldError::RsLsfEmptyKey);
    }
    if target_field_key.len() >= RSLSF_MAX_LINE_BYTES {
        return Err(ReadTomlFieldError::RsLsfKeyTooLong);
    }

    // ----------------------------------------------------------------------
    // Open the file (terse error: no path leakage)
    // ----------------------------------------------------------------------
    let mut open_file_handle: File = match File::open(absolute_toml_file_path) {
        Ok(handle) => handle,
        Err(_) => return Err(ReadTomlFieldError::RsLsfFileOpenFailed),
    };

    // ----------------------------------------------------------------------
    // Stack-allocated scratch buffers
    // ----------------------------------------------------------------------
    let mut chunk_read_buffer: [u8; RSLSF_READ_CHUNK_BYTES] = [0u8; RSLSF_READ_CHUNK_BYTES];
    let mut current_line_buffer: [u8; RSLSF_MAX_LINE_BYTES] = [0u8; RSLSF_MAX_LINE_BYTES];
    let mut current_line_length: usize = 0;
    let mut current_line_overflowed_buffer: bool = false;
    let mut cumulative_bytes_scanned: u64 = 0;

    // Iteration failsafe: even if `read` were to misbehave and always return 1
    // byte, this caps the outer loop. (Belt-and-suspenders with the byte cap.)
    let mut safety_iteration_count: u64 = 0;
    let safety_iteration_limit: u64 =
        (RSLSF_MAX_BYTES_SCANNED / (RSLSF_READ_CHUNK_BYTES as u64)) + 16;

    // ----------------------------------------------------------------------
    // Read loop
    // ----------------------------------------------------------------------
    loop {
        safety_iteration_count = safety_iteration_count.saturating_add(1);
        if safety_iteration_count > safety_iteration_limit {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        let bytes_read_this_chunk: usize = match open_file_handle.read(&mut chunk_read_buffer) {
            Ok(count) => count,
            Err(_) => return Err(ReadTomlFieldError::RsLsfFileReadFailed),
        };

        // ------------------------------------------------------------------
        // End-of-file: process any final unterminated line, then report.
        // ------------------------------------------------------------------
        if bytes_read_this_chunk == 0 {
            if current_line_overflowed_buffer {
                // If the overflowing trailing line *could* have been our key,
                // it is unsafe to silently skip it: report explicitly.
                if line_prefix_could_match_key(
                    &current_line_buffer[..current_line_length],
                    target_field_key,
                ) {
                    return Err(ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer);
                }
            } else if current_line_length > 0 {
                match try_match_line_against_key::<OUTPUT_BUFFER_BYTES>(
                    &current_line_buffer[..current_line_length],
                    target_field_key,
                )? {
                    Some(found_value_tuple) => return Ok(found_value_tuple),
                    None => {}
                }
            }
            return Err(ReadTomlFieldError::RsLsfFieldNotFound);
        }

        cumulative_bytes_scanned =
            cumulative_bytes_scanned.saturating_add(bytes_read_this_chunk as u64);
        if cumulative_bytes_scanned > RSLSF_MAX_BYTES_SCANNED {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        // ------------------------------------------------------------------
        // Byte-by-byte line accumulator. Bounded by `bytes_read_this_chunk`
        // (always <= RSLSF_READ_CHUNK_BYTES), so this inner loop is bounded.
        // ------------------------------------------------------------------
        let mut byte_index_in_chunk: usize = 0;
        while byte_index_in_chunk < bytes_read_this_chunk {
            let current_byte: u8 = chunk_read_buffer[byte_index_in_chunk];
            byte_index_in_chunk += 1;

            match current_byte {
                b'\n' => {
                    if current_line_overflowed_buffer {
                        if line_prefix_could_match_key(
                            &current_line_buffer[..current_line_length],
                            target_field_key,
                        ) {
                            return Err(ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer);
                        }
                        // Otherwise: this unrelated long line cannot affect us;
                        // continue scanning the file.
                    } else if current_line_length > 0 {
                        match try_match_line_against_key::<OUTPUT_BUFFER_BYTES>(
                            &current_line_buffer[..current_line_length],
                            target_field_key,
                        )? {
                            Some(found_value_tuple) => return Ok(found_value_tuple),
                            None => {}
                        }
                    }
                    current_line_length = 0;
                    current_line_overflowed_buffer = false;
                }
                b'\r' => {
                    // Drop CR so CRLF and LF line endings are both handled.
                    // A bare CR (old Mac line endings) is also dropped; not
                    // a supported terminator per the in-scope policy above.
                }
                _ => {
                    if current_line_length < RSLSF_MAX_LINE_BYTES {
                        current_line_buffer[current_line_length] = current_byte;
                        current_line_length += 1;
                    } else {
                        // Overflow: stop accumulating but keep the prefix so
                        // we can decide at line end whether overflow matters.
                        current_line_overflowed_buffer = true;
                    }
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Internal helpers (pure, stateless, no heap)
// ----------------------------------------------------------------------------

/// Attempt to match a single fully-accumulated line against `target_field_key`.
///
/// Returns:
/// * `Ok(Some((buf, len)))` — line matched the key and the value fit.
/// * `Ok(None)`              — line did not match the key (skip and continue).
/// * `Err(...)`              — line matched the key but the value will not
///                              fit in `OUTPUT_BUFFER_BYTES`.
fn try_match_line_against_key<const OUTPUT_BUFFER_BYTES: usize>(
    raw_line_bytes: &[u8],
    target_field_key: &str,
) -> Result<Option<([u8; OUTPUT_BUFFER_BYTES], usize)>, ReadTomlFieldError> {
    let trimmed_line_bytes: &[u8] = trim_ascii_whitespace(raw_line_bytes);

    // Empty lines and full-line comments cannot be key-value pairs.
    if trimmed_line_bytes.is_empty() {
        return Ok(None);
    }
    if trimmed_line_bytes[0] == b'#' {
        return Ok(None);
    }

    let key_bytes: &[u8] = target_field_key.as_bytes();
    if trimmed_line_bytes.len() < key_bytes.len() {
        return Ok(None);
    }

    // The line must begin with the exact key bytes...
    if &trimmed_line_bytes[..key_bytes.len()] != key_bytes {
        return Ok(None);
    }

    // ...followed by optional whitespace and a single '='. This prevents
    // partial-prefix collisions such as key "name" against line "name_long".
    let post_key_bytes: &[u8] = &trimmed_line_bytes[key_bytes.len()..];
    let mut cursor_position: usize = 0;
    while cursor_position < post_key_bytes.len()
        && is_ascii_space_or_tab_byte(post_key_bytes[cursor_position])
    {
        cursor_position += 1;
    }
    if cursor_position >= post_key_bytes.len() || post_key_bytes[cursor_position] != b'=' {
        return Ok(None);
    }
    cursor_position += 1;
    while cursor_position < post_key_bytes.len()
        && is_ascii_space_or_tab_byte(post_key_bytes[cursor_position])
    {
        cursor_position += 1;
    }

    // Strip a single pair of surrounding double quotes, if present.
    let raw_value_bytes: &[u8] = &post_key_bytes[cursor_position..];
    let stripped_value_bytes: &[u8] = strip_surrounding_double_quotes(raw_value_bytes);

    if stripped_value_bytes.len() > OUTPUT_BUFFER_BYTES {
        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
    }

    let mut output_buffer: [u8; OUTPUT_BUFFER_BYTES] = [0u8; OUTPUT_BUFFER_BYTES];
    output_buffer[..stripped_value_bytes.len()].copy_from_slice(stripped_value_bytes);
    Ok(Some((output_buffer, stripped_value_bytes.len())))
}

/// True iff, after leading-whitespace trim, `raw_line_bytes` starts with the
/// exact bytes of `candidate_key`. Used solely to decide whether an overflowed
/// line could have been the line we cared about.
fn line_prefix_could_match_key(raw_line_bytes: &[u8], candidate_key: &str) -> bool {
    let mut leading_index: usize = 0;
    while leading_index < raw_line_bytes.len()
        && is_ascii_space_or_tab_byte(raw_line_bytes[leading_index])
    {
        leading_index += 1;
    }
    let post_whitespace_bytes: &[u8] = &raw_line_bytes[leading_index..];
    let key_bytes: &[u8] = candidate_key.as_bytes();
    if post_whitespace_bytes.len() < key_bytes.len() {
        return false;
    }
    &post_whitespace_bytes[..key_bytes.len()] == key_bytes
}

/// Trim ASCII whitespace (space, tab, CR, LF) from both ends, without
/// allocating. Returns a sub-slice of the input.
fn trim_ascii_whitespace(input_bytes: &[u8]) -> &[u8] {
    let mut start_index: usize = 0;
    let mut end_index: usize = input_bytes.len();
    while start_index < end_index && is_ascii_whitespace_byte(input_bytes[start_index]) {
        start_index += 1;
    }
    while end_index > start_index && is_ascii_whitespace_byte(input_bytes[end_index - 1]) {
        end_index -= 1;
    }
    &input_bytes[start_index..end_index]
}

#[inline]
fn is_ascii_whitespace_byte(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t' | b'\r' | b'\n')
}

#[inline]
fn is_ascii_space_or_tab_byte(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t')
}

/// If `input_bytes` is at least two bytes long and both first and last bytes
/// are `"`, return the inner slice; otherwise return `input_bytes` unchanged.
/// Does not handle escape sequences — out of scope for this module.
fn strip_surrounding_double_quotes(input_bytes: &[u8]) -> &[u8] {
    if input_bytes.len() >= 2
        && input_bytes[0] == b'"'
        && input_bytes[input_bytes.len() - 1] == b'"'
    {
        &input_bytes[1..input_bytes.len() - 1]
    } else {
        input_bytes
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------
#[cfg(test)]
mod rslsf_tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// Helper: write a fresh temp file with the given contents and return its
    /// absolute path. Test-only; may use heap/panic-on-failure freely.
    fn write_unique_temp_toml(label: &str, contents: &str) -> PathBuf {
        let mut path_buffer = std::env::temp_dir();
        // Make each test file name unique to avoid cross-test interference
        // even when tests run in parallel.
        let unique_suffix = format!(
            "{}_{}_{}",
            std::process::id(),
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        path_buffer.push(format!("rslsf_test_{}.toml", unique_suffix));
        let mut created_file =
            std::fs::File::create(&path_buffer).expect("test setup: create temp file");
        created_file
            .write_all(contents.as_bytes())
            .expect("test setup: write temp file");
        path_buffer
    }

    fn path_as_str(path: &PathBuf) -> &str {
        path.to_str().expect("test setup: temp path must be UTF-8")
    }

    #[test]
    fn rslsf_finds_simple_quoted_value() {
        let test_path = write_unique_temp_toml("simple_quoted", "name = \"alice\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"alice");
    }

    #[test]
    fn rslsf_finds_unquoted_value() {
        let test_path = write_unique_temp_toml("unquoted", "port = 8080\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"8080");
    }

    #[test]
    fn rslsf_handles_crlf_endings() {
        let test_path = write_unique_temp_toml("crlf", "name = \"bob\"\r\nother = \"x\"\r\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"bob");
    }

    #[test]
    fn rslsf_skips_comments_and_blank_lines() {
        let test_path = write_unique_temp_toml(
            "comments",
            "# a header comment\n\n   # indented comment\nname = \"carol\"\n",
        );
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"carol");
    }

    #[test]
    fn rslsf_does_not_match_key_with_extra_prefix_chars() {
        // "name_long" must NOT be accepted when caller asked for "name".
        let test_path = write_unique_temp_toml(
            "prefix_collision",
            "name_long = \"WRONG\"\nname = \"RIGHT\"\n",
        );
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"RIGHT");
    }

    #[test]
    fn rslsf_returns_field_not_found_when_missing() {
        let test_path = write_unique_temp_toml("missing", "other = \"x\"\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name");
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfFieldNotFound));
    }

    #[test]
    fn rslsf_returns_value_too_long_when_output_too_small() {
        // "toolongxx" is 9 bytes, which exceeds the 8-byte output buffer.
        let test_path = write_unique_temp_toml("too_long", "name = \"toolongxx\"\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<8>(path_as_str(&test_path), "name");
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer)
        );
    }

    #[test]
    fn rslsf_returns_open_failed_for_nonexistent_path() {
        // Path under temp_dir that we have not created. Avoid hard-coding
        // platform-specific absolute paths (e.g. "/nope/...") so the test
        // is portable to Windows runners.
        let mut bogus_path = std::env::temp_dir();
        bogus_path.push("rslsf_test_definitely_does_not_exist_xyzzy_12345.toml");
        let result = read_single_line_string_field_from_toml_no_heap::<16>(
            bogus_path
                .to_str()
                .expect("test setup: temp path must be UTF-8"),
            "name",
        );
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfFileOpenFailed));
    }

    #[test]
    fn rslsf_rejects_empty_key() {
        let test_path = write_unique_temp_toml("empty_key", "name = \"x\"\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "");
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfEmptyKey));
    }

    #[test]
    fn rslsf_rejects_key_too_long() {
        let test_path = write_unique_temp_toml("key_too_long", "name = \"x\"\n");
        // Build a key longer than RSLSF_MAX_LINE_BYTES without using `vec!`
        // anywhere in production code — this is test-only setup.
        let oversized_key: String = "k".repeat(RSLSF_MAX_LINE_BYTES + 1);
        let result = read_single_line_string_field_from_toml_no_heap::<16>(
            path_as_str(&test_path),
            &oversized_key,
        );
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfKeyTooLong));
    }

    #[test]
    fn rslsf_handles_no_trailing_newline() {
        // No final '\n' — the last line must still be processed at EOF.
        let test_path = write_unique_temp_toml("no_trailing_lf", "name = \"dora\"");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value at EOF without newline");
        assert_eq!(&output_buffer[..written_length], b"dora");
    }

    #[test]
    fn rslsf_finds_key_after_many_unrelated_lines() {
        // Force the scanner to cross several read-chunk boundaries before
        // it sees the target key. This exercises the chunk/line-accumulator
        // boundary logic.
        let mut contents = String::new();
        for i in 0..50 {
            // Each line ~30 bytes; 50 lines ~ 1500 bytes, well over one chunk.
            contents.push_str(&format!("noise_key_{:03} = \"junkjunkjunk\"\n", i));
        }
        contents.push_str("target = \"eve\"\n");
        let test_path = write_unique_temp_toml("many_lines", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(
                path_as_str(&test_path),
                "target",
            )
            .expect("should find value across chunk boundaries");
        assert_eq!(&output_buffer[..written_length], b"eve");
    }

    #[test]
    fn rslsf_unrelated_long_line_does_not_abort_scan() {
        // A line far longer than RSLSF_MAX_LINE_BYTES that is NOT the key
        // we are looking for must be silently skipped, not aborted, so the
        // real key further down in the file is still found.
        let oversized_unrelated_line: String = std::iter::once("other_key = \"")
            .chain(std::iter::repeat("X").take(RSLSF_MAX_LINE_BYTES + 64))
            .chain(std::iter::once("\"\n"))
            .collect();
        let mut contents = String::new();
        contents.push_str(&oversized_unrelated_line);
        contents.push_str("name = \"frank\"\n");
        let test_path = write_unique_temp_toml("unrelated_overflow", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("unrelated overflowing line should not block finding the real key");
        assert_eq!(&output_buffer[..written_length], b"frank");
    }

    #[test]
    fn rslsf_matching_line_overflow_is_reported_not_truncated() {
        // The TARGET key's line overflows the scan buffer. We must NOT
        // silently truncate and return a bogus value; we must report the
        // overflow explicitly.
        let mut contents = String::new();
        contents.push_str("name = \"");
        for _ in 0..(RSLSF_MAX_LINE_BYTES + 32) {
            contents.push('Z');
        }
        contents.push_str("\"\n");
        let test_path = write_unique_temp_toml("matching_overflow", &contents);
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name");
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer)
        );
    }

    #[test]
    fn rslsf_handles_whitespace_around_key_and_equals() {
        // Both leading whitespace and varied spacing around '=' must work.
        let test_path = write_unique_temp_toml("whitespace", "   name\t=\t  \"grace\"   \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value with varied whitespace");
        assert_eq!(&output_buffer[..written_length], b"grace");
    }

    #[test]
    fn rslsf_first_match_wins_when_key_appears_twice() {
        // Document the precedence policy: first match wins. If your project
        // needs last-match-wins, change this test AND the function's docs
        // together — don't let the two drift apart.
        let test_path =
            write_unique_temp_toml("duplicate_key", "name = \"first\"\nname = \"second\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find first value");
        assert_eq!(&output_buffer[..written_length], b"first");
    }

    #[test]
    fn rslsf_zero_sized_output_buffer_is_rejected() {
        // We cannot exercise this via the public generic with `::<0>` and
        // get a meaningful value back, but we can confirm the production
        // catch-handle exists and triggers BEFORE any I/O occurs. We pass
        // a deliberately bogus path to prove that the parameter check fires
        // first (we get OutputBufferZeroSized, NOT FileOpenFailed).
        let mut bogus_path = std::env::temp_dir();
        bogus_path.push("rslsf_test_zero_buffer_should_not_open.toml");
        let result = read_single_line_string_field_from_toml_no_heap::<0>(
            bogus_path
                .to_str()
                .expect("test setup: temp path must be UTF-8"),
            "name",
        );
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized));
    }
}

// ============================================================================
// SECTION 34: Memo File Readers — Constants
// ============================================================================

/// Maximum bytes of a `text_message` value in a bootstrap config memo file.
///
/// ## Sizing Rationale
///
/// Config lines have the form `key:value`. The longest expected line is
/// `plays_white:` (12 bytes) followed by a maximum-length username
/// (`MAX_USERNAME_BYTES` = 16), totaling 28 bytes. 32 bytes gives
/// comfortable headroom without waste.
///
/// A `text_message` value longer than this is treated as "not a config
/// line we recognize" and the file is skipped silently.
pub const MAX_CONFIG_TEXT_MESSAGE_BYTES: usize = 32;

/// Maximum bytes of a `text_message` value in a game-loop move memo file.
///
/// ## Sizing Rationale
///
/// Legal chess notation fits in 9 bytes (see `NOTATION_NORMALIZED_BUFFER_BYTES`).
/// Non-move commands `draw` (4 bytes) and `resign` (6 bytes) also fit
/// trivially. 16 bytes covers all forms with headroom for any annotation
/// suffix combinations users might include.
pub const MAX_MOVE_TEXT_MESSAGE_BYTES: usize = 16;

/// Maximum bytes of the ASCII-decimal representation of a Unix timestamp
/// as it appears in a TOML file's `updated_at_timestamp = "..."` field.
///
/// ## Sizing Rationale
///
/// `u64::MAX` is `18446744073709551615`, which is 20 decimal digits.
/// 20 bytes is the exact maximum; we use it as the scratch buffer size
/// for reading the field before parsing.
pub const MAX_TIMESTAMP_DECIMAL_BYTES: usize = 20;

// ============================================================================
// SECTION 35: Memo File Readers — Error Types
// ============================================================================

/// Failure modes specific to `read_memo_config_file`.
///
/// All variants are unit variants per project policy: no embedded data
/// can leak into production logs. Skip conditions (missing field, value
/// too long) are NOT errors — they are represented by `Ok(None)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoConfigReadError {
    /// The caller supplied an empty `absolute_file_path` string.
    EmptyFilePath,
    /// The underlying TOML primitive could not open the file.
    /// Includes "file does not exist" and any permission/OS failure.
    FileOpenFailed,
    /// The underlying TOML primitive failed mid-read.
    FileReadFailed,
    /// Defensive backstop: an internal invariant was violated.
    /// Theoretically unreachable via the public API.
    InternalReaderFault,
}

/// Failure modes specific to `read_memo_move_file`.
///
/// All variants are unit variants per project policy. As with
/// `MemoConfigReadError`, "skip this file" outcomes are represented by
/// `Ok(None)`, not by error variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoMoveReadError {
    /// The caller supplied an empty `absolute_file_path` string.
    EmptyFilePath,
    /// The underlying TOML primitive could not open the file.
    FileOpenFailed,
    /// The underlying TOML primitive failed mid-read.
    FileReadFailed,
    /// Defensive backstop: an internal invariant was violated.
    InternalReaderFault,
}

// ============================================================================
// SECTION 36: Memo File Readers — Structs
// ============================================================================

/// Contents of one TOML memo file as needed by the bootstrap (Q&A) layer.
///
/// ## Project Context
///
/// Bootstrap iterates the memo directory looking for configuration values
/// (`plays_white:alice`, `refresh_rate:10`, etc.). For each file, it
/// reads only `text_message`. Owner and timestamp are ignored because
/// any user may supply config; first valid value wins; order does not
/// matter.
///
/// ## Storage
///
/// Fixed-size buffer with explicit length. No heap, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoConfigFileContents {
    /// Raw bytes of the `text_message` field value.
    /// Meaningful bytes occupy `text_message_buffer[..text_message_length]`.
    pub text_message_buffer: [u8; MAX_CONFIG_TEXT_MESSAGE_BYTES],
    /// Number of meaningful bytes in `text_message_buffer`.
    /// In range `0..=MAX_CONFIG_TEXT_MESSAGE_BYTES`.
    pub text_message_length: u8,
}

impl MemoConfigFileContents {
    /// Borrow the meaningful prefix of `text_message_buffer` as a slice.
    pub fn text_message_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.text_message_length as usize;
        // Defensive clamp; cannot exceed buffer length via the public API.
        let safe_length = if length_as_usize > MAX_CONFIG_TEXT_MESSAGE_BYTES {
            MAX_CONFIG_TEXT_MESSAGE_BYTES
        } else {
            length_as_usize
        };
        &self.text_message_buffer[..safe_length]
    }
}

/// Contents of one TOML memo file as needed by the game-loop layer.
///
/// ## Project Context
///
/// The game loop scans the memo directory in chronological order, looking
/// for the next file that contains a valid move from the player whose
/// turn it is. For each candidate file, it requires:
///
/// - `owner`: the player who wrote this memo
/// - `text_message`: the move notation (or `draw` / `resign`)
/// - `updated_at_timestamp`: a Unix timestamp (seconds since epoch)
///
/// If any of these is missing, the file is skipped silently. This
/// struct represents a file that has all three.
///
/// ## TOML format expected
///
/// ```toml
/// owner = "alice"
/// text_message = "Nf3"
/// updated_at_timestamp = "1778532301"
/// ```
///
/// The timestamp must be a **quoted string** containing only ASCII
/// decimal digits. Bare-integer TOML form (`updated_at_timestamp = 1778532301`)
/// is NOT accepted by this reader, because the underlying single-field
/// primitive extracts string values only.
///
/// ## Storage
///
/// Fixed-size buffers with explicit lengths. No heap, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoMoveFileContents {
    /// Raw bytes of the `owner` field value.
    /// Meaningful bytes occupy `owner_buffer[..owner_length]`.
    pub owner_buffer: [u8; MAX_USERNAME_BYTES],
    /// Number of meaningful bytes in `owner_buffer`.
    /// In range `1..=MAX_USERNAME_BYTES`.
    pub owner_length: u8,

    /// Raw bytes of the `text_message` field value.
    /// Meaningful bytes occupy `text_message_buffer[..text_message_length]`.
    pub text_message_buffer: [u8; MAX_MOVE_TEXT_MESSAGE_BYTES],
    /// Number of meaningful bytes in `text_message_buffer`.
    /// In range `1..=MAX_MOVE_TEXT_MESSAGE_BYTES`.
    pub text_message_length: u8,

    /// Parsed Unix timestamp (seconds since epoch).
    pub updated_at_unix_timestamp: u64,
}

impl MemoMoveFileContents {
    /// Borrow the meaningful prefix of `owner_buffer` as a slice.
    pub fn owner_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.owner_length as usize;
        let safe_length = if length_as_usize > MAX_USERNAME_BYTES {
            MAX_USERNAME_BYTES
        } else {
            length_as_usize
        };
        &self.owner_buffer[..safe_length]
    }

    /// Borrow the meaningful prefix of `text_message_buffer` as a slice.
    pub fn text_message_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.text_message_length as usize;
        let safe_length = if length_as_usize > MAX_MOVE_TEXT_MESSAGE_BYTES {
            MAX_MOVE_TEXT_MESSAGE_BYTES
        } else {
            length_as_usize
        };
        &self.text_message_buffer[..safe_length]
    }
}

// ============================================================================
// SECTION 37: Memo File Readers — Internal Helper (Decimal Parse)
// ============================================================================

/// Parse an ASCII-decimal byte slice into a `u64`.
///
/// ## Accepted Input
///
/// One or more bytes, each in `b'0'..=b'9'`. No leading sign, no leading
/// `+`, no leading whitespace, no internal separators, no trailing junk.
/// (The TOML primitive trims surrounding quotes already; whitespace
/// stripping is not this function's responsibility.)
///
/// ## Returns
///
/// `Some(value)` on successful parse. `None` for:
/// - Empty input.
/// - Any byte outside `b'0'..=b'9'`.
/// - Overflow beyond `u64::MAX`.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Bounded loop (one iteration per input byte).
fn parse_decimal_u64_from_ascii_bytes(input_bytes: &[u8]) -> Option<u64> {
    if input_bytes.is_empty() {
        return None;
    }

    let mut accumulator: u64 = 0;
    let mut byte_index: usize = 0;
    while byte_index < input_bytes.len() {
        let current_byte = input_bytes[byte_index];
        if current_byte < b'0' || current_byte > b'9' {
            return None;
        }
        let digit_value: u64 = (current_byte - b'0') as u64;

        // accumulator = accumulator * 10 + digit_value, checked for overflow.
        let multiplied = match accumulator.checked_mul(10) {
            Some(value) => value,
            None => return None,
        };
        let added = match multiplied.checked_add(digit_value) {
            Some(value) => value,
            None => return None,
        };
        accumulator = added;

        byte_index += 1;
    }

    Some(accumulator)
}

// ============================================================================
// SECTION 38: Memo File Readers — Internal Helper (Map Primitive Error)
// ============================================================================

/// Classify a `ReadTomlFieldError` from the underlying TOML primitive into
/// one of three outcomes for the bootstrap/game-loop readers:
///
/// - `MapPrimitiveOutcome::Skip` — the field is absent or too long for our
///   buffer; the caller should treat this as "skip this file".
/// - `MapPrimitiveOutcome::IoError(ReaderIoErrorKind)` — a real I/O failure;
///   the caller should surface this as an `Err(...)`.
/// - `MapPrimitiveOutcome::Programmer(ReaderProgrammerErrorKind)` — the
///   caller supplied invalid arguments (empty key, etc.); the caller
///   should surface this as an `Err(...)`.
///
/// This helper exists so that both `read_memo_config_file` and
/// `read_memo_move_file` share the same classification policy without
/// duplicating match arms.
fn classify_primitive_read_error(primitive_error: ReadTomlFieldError) -> ClassifiedPrimitiveError {
    match primitive_error {
        // Field not present, or its value is too large for our caller's
        // chosen buffer. Both are "skip this file" conditions.
        ReadTomlFieldError::RsLsfFieldNotFound
        | ReadTomlFieldError::RsLsfValueExceedsOutputBuffer
        | ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer => ClassifiedPrimitiveError::Skip,
        // Real I/O failures.
        ReadTomlFieldError::RsLsfFileOpenFailed => ClassifiedPrimitiveError::FileOpenFailed,
        ReadTomlFieldError::RsLsfFileReadFailed => ClassifiedPrimitiveError::FileReadFailed,
        // Programmer error or defensive backstop. These reflect bugs at
        // the call site (empty/oversize key) or unreachable conditions.
        ReadTomlFieldError::RsLsfEmptyKey
        | ReadTomlFieldError::RsLsfKeyTooLong
        | ReadTomlFieldError::RsLsfOutputBufferZeroSized
        | ReadTomlFieldError::RsLsfSafetyBudgetExhausted => {
            ClassifiedPrimitiveError::InternalReaderFault
        }
    }
}

/// Outcome of classifying a primitive read error. Internal to the reader
/// module; not exposed to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassifiedPrimitiveError {
    Skip,
    FileOpenFailed,
    FileReadFailed,
    InternalReaderFault,
}

// ============================================================================
// SECTION 39: Memo File Readers — Bootstrap Reader
// ============================================================================

/// Reads a single memo TOML file for the bootstrap (Q&A config) layer.
///
/// ## Project Context
///
/// Called once per file during bootstrap directory iteration. The caller
/// iterates the directory in any order (chronological order is irrelevant
/// for config collection) and asks this function for the `text_message`
/// field of each file. If `Ok(Some(_))`, the caller passes the bytes to
/// the config-line parser. If `Ok(None)`, the caller moves on to the
/// next file.
///
/// ## Arguments
///
/// - `absolute_file_path`: absolute path to a TOML file as a `&str`.
///   Must be non-empty.
///
/// ## Returns
///
/// - `Ok(Some(MemoConfigFileContents))`: the file was opened, read, and
///   contained a `text_message` field whose value fits in
///   `MAX_CONFIG_TEXT_MESSAGE_BYTES`.
/// - `Ok(None)`: the file was opened and read, but either had no
///   `text_message` field, or the value did not fit. Skip silently.
/// - `Err(MemoConfigReadError::EmptyFilePath)`: caller bug.
/// - `Err(MemoConfigReadError::FileOpenFailed)`: I/O failure opening.
/// - `Err(MemoConfigReadError::FileReadFailed)`: I/O failure during read.
/// - `Err(MemoConfigReadError::InternalReaderFault)`: defensive backstop.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Stack-allocated read buffer of
/// `MAX_CONFIG_TEXT_MESSAGE_BYTES` bytes.
pub fn read_memo_config_file(
    absolute_file_path: &str,
) -> Result<Option<MemoConfigFileContents>, MemoConfigReadError> {
    if absolute_file_path.is_empty() {
        return Err(MemoConfigReadError::EmptyFilePath);
    }

    let read_result = read_single_line_string_field_from_toml_no_heap::<
        MAX_CONFIG_TEXT_MESSAGE_BYTES,
    >(absolute_file_path, "text_message");

    match read_result {
        Ok((output_buffer, written_length)) => {
            // `written_length` is guaranteed by the primitive to be
            // <= MAX_CONFIG_TEXT_MESSAGE_BYTES. We narrow to u8 with a
            // defensive check; the buffer constant is 32, which fits.
            if written_length > MAX_CONFIG_TEXT_MESSAGE_BYTES {
                return Err(MemoConfigReadError::InternalReaderFault);
            }
            Ok(Some(MemoConfigFileContents {
                text_message_buffer: output_buffer,
                text_message_length: written_length as u8,
            }))
        }
        Err(primitive_error) => match classify_primitive_read_error(primitive_error) {
            ClassifiedPrimitiveError::Skip => Ok(None),
            ClassifiedPrimitiveError::FileOpenFailed => Err(MemoConfigReadError::FileOpenFailed),
            ClassifiedPrimitiveError::FileReadFailed => Err(MemoConfigReadError::FileReadFailed),
            ClassifiedPrimitiveError::InternalReaderFault => {
                Err(MemoConfigReadError::InternalReaderFault)
            }
        },
    }
}

// ============================================================================
// SECTION 40: Memo File Readers — Game-Loop Reader
// ============================================================================

/// Reads a single memo TOML file for the game-loop (move processing) layer.
///
/// ## Project Context
///
/// Called by the game-loop file-scanning layer once per candidate file
/// in chronological order. The caller passes the absolute path; this
/// function reads `owner`, `text_message`, and `updated_at_timestamp`,
/// returning a fully populated struct if all three are present and
/// well-formed, or `Ok(None)` if any field is missing, too long, or
/// (for the timestamp) not a valid ASCII decimal number.
///
/// The caller is responsible for further filtering — for example,
/// checking that `owner` matches the player whose turn it is.
///
/// ## Arguments
///
/// - `absolute_file_path`: absolute path to a TOML file. Must be non-empty.
///
/// ## Returns
///
/// - `Ok(Some(MemoMoveFileContents))`: all three fields present and
///   well-formed.
/// - `Ok(None)`: at least one required field missing, value too long,
///   or timestamp malformed. Skip silently.
/// - `Err(MemoMoveReadError::EmptyFilePath)`: caller bug.
/// - `Err(MemoMoveReadError::FileOpenFailed)`: I/O failure opening.
/// - `Err(MemoMoveReadError::FileReadFailed)`: I/O failure during read.
/// - `Err(MemoMoveReadError::InternalReaderFault)`: defensive backstop.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Three stack-allocated read buffers totaling
/// `MAX_USERNAME_BYTES + MAX_MOVE_TEXT_MESSAGE_BYTES + MAX_TIMESTAMP_DECIMAL_BYTES`
/// bytes (52 bytes for current constants).
///
/// ## Sequencing
///
/// The three fields are read in this order: `owner`, `text_message`,
/// `updated_at_timestamp`. If any read returns a "skip" condition, the
/// function returns `Ok(None)` without reading the remaining fields.
/// If any read returns a hard I/O failure, that failure is surfaced
/// immediately. This short-circuit behavior bounds the I/O cost of
/// invalid files.
pub fn read_memo_move_file(
    absolute_file_path: &str,
) -> Result<Option<MemoMoveFileContents>, MemoMoveReadError> {
    if absolute_file_path.is_empty() {
        return Err(MemoMoveReadError::EmptyFilePath);
    }

    // ── Field 1: owner ─────────────────────────────────────────────────
    let owner_read_result = read_single_line_string_field_from_toml_no_heap::<MAX_USERNAME_BYTES>(
        absolute_file_path,
        "owner",
    );

    let (owner_buffer, owner_written_length) = match owner_read_result {
        Ok(pair) => pair,
        Err(primitive_error) => match classify_primitive_read_error(primitive_error) {
            ClassifiedPrimitiveError::Skip => return Ok(None),
            ClassifiedPrimitiveError::FileOpenFailed => {
                return Err(MemoMoveReadError::FileOpenFailed);
            }
            ClassifiedPrimitiveError::FileReadFailed => {
                return Err(MemoMoveReadError::FileReadFailed);
            }
            ClassifiedPrimitiveError::InternalReaderFault => {
                return Err(MemoMoveReadError::InternalReaderFault);
            }
        },
    };

    // Reject empty owner values: a present-but-empty owner field is not
    // useful to the game-loop ("owner = \"\""). Skip the file.
    if owner_written_length == 0 {
        return Ok(None);
    }
    if owner_written_length > MAX_USERNAME_BYTES {
        return Err(MemoMoveReadError::InternalReaderFault);
    }

    // ── Field 2: text_message ──────────────────────────────────────────
    let text_message_read_result = read_single_line_string_field_from_toml_no_heap::<
        MAX_MOVE_TEXT_MESSAGE_BYTES,
    >(absolute_file_path, "text_message");

    let (text_message_buffer, text_message_written_length) = match text_message_read_result {
        Ok(pair) => pair,
        Err(primitive_error) => match classify_primitive_read_error(primitive_error) {
            ClassifiedPrimitiveError::Skip => return Ok(None),
            ClassifiedPrimitiveError::FileOpenFailed => {
                return Err(MemoMoveReadError::FileOpenFailed);
            }
            ClassifiedPrimitiveError::FileReadFailed => {
                return Err(MemoMoveReadError::FileReadFailed);
            }
            ClassifiedPrimitiveError::InternalReaderFault => {
                return Err(MemoMoveReadError::InternalReaderFault);
            }
        },
    };

    if text_message_written_length == 0 {
        return Ok(None);
    }
    if text_message_written_length > MAX_MOVE_TEXT_MESSAGE_BYTES {
        return Err(MemoMoveReadError::InternalReaderFault);
    }

    // ── Field 3: updated_at_timestamp (string → u64) ───────────────────
    let timestamp_read_result = read_single_line_string_field_from_toml_no_heap::<
        MAX_TIMESTAMP_DECIMAL_BYTES,
    >(absolute_file_path, "updated_at_timestamp");

    let (timestamp_buffer, timestamp_written_length) = match timestamp_read_result {
        Ok(pair) => pair,
        Err(primitive_error) => match classify_primitive_read_error(primitive_error) {
            ClassifiedPrimitiveError::Skip => return Ok(None),
            ClassifiedPrimitiveError::FileOpenFailed => {
                return Err(MemoMoveReadError::FileOpenFailed);
            }
            ClassifiedPrimitiveError::FileReadFailed => {
                return Err(MemoMoveReadError::FileReadFailed);
            }
            ClassifiedPrimitiveError::InternalReaderFault => {
                return Err(MemoMoveReadError::InternalReaderFault);
            }
        },
    };

    if timestamp_written_length == 0 {
        return Ok(None);
    }
    if timestamp_written_length > MAX_TIMESTAMP_DECIMAL_BYTES {
        return Err(MemoMoveReadError::InternalReaderFault);
    }

    let timestamp_bytes_slice = &timestamp_buffer[..timestamp_written_length];
    let parsed_timestamp = match parse_decimal_u64_from_ascii_bytes(timestamp_bytes_slice) {
        Some(value) => value,
        None => return Ok(None), // malformed timestamp → skip
    };

    Ok(Some(MemoMoveFileContents {
        owner_buffer,
        owner_length: owner_written_length as u8,
        text_message_buffer,
        text_message_length: text_message_written_length as u8,
        updated_at_unix_timestamp: parsed_timestamp,
    }))
}

// ============================================================================
// SECTION 41: Memo File Readers — Cargo Tests
// ============================================================================

#[cfg(test)]
mod tests_memo_file_readers {
    //! ## Test Strategy
    //!
    //! Each test writes a fixture TOML file to the OS temp directory with
    //! a unique filename (incorporating the test name) and then exercises
    //! the reader against that absolute path. The file is removed at the
    //! end of each test via `std::fs::remove_file` in a final cleanup step;
    //! a failure to remove is non-fatal (logged as a test eprintln).
    //!
    //! Test files use names like `memo_reader_test_<name>.toml` to avoid
    //! collisions when tests run in parallel.

    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;

    /// Build a temp-file path with a unique filename for this test.
    fn build_test_fixture_path(unique_test_name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("memo_reader_test_{}.toml", unique_test_name));
        path
    }

    /// Write `contents` to `absolute_path`, returning a guard that
    /// removes the file when dropped. The guard makes per-test cleanup
    /// automatic and panic-safe (drop runs even on assertion failure).
    struct FixtureFile {
        path: PathBuf,
    }

    impl FixtureFile {
        fn create(unique_test_name: &str, contents: &str) -> FixtureFile {
            let path = build_test_fixture_path(unique_test_name);
            let mut file =
                File::create(&path).expect("test: must be able to create temp fixture file");
            file.write_all(contents.as_bytes())
                .expect("test: must be able to write temp fixture contents");
            FixtureFile { path }
        }

        fn path_as_str(&self) -> &str {
            self.path.to_str().expect("test: temp path must be UTF-8")
        }
    }

    impl Drop for FixtureFile {
        fn drop(&mut self) {
            if let Err(io_err) = std::fs::remove_file(&self.path) {
                eprintln!(
                    "test cleanup: failed to remove fixture file (non-fatal): {:?}",
                    io_err
                );
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Tests for parse_decimal_u64_from_ascii_bytes
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn parse_decimal_accepts_zero() {
        assert_eq!(parse_decimal_u64_from_ascii_bytes(b"0"), Some(0));
    }

    #[test]
    fn parse_decimal_accepts_small_value() {
        assert_eq!(parse_decimal_u64_from_ascii_bytes(b"42"), Some(42));
    }

    #[test]
    fn parse_decimal_accepts_realistic_timestamp() {
        assert_eq!(
            parse_decimal_u64_from_ascii_bytes(b"1778532301"),
            Some(1_778_532_301)
        );
    }

    #[test]
    fn parse_decimal_accepts_u64_max() {
        assert_eq!(
            parse_decimal_u64_from_ascii_bytes(b"18446744073709551615"),
            Some(u64::MAX)
        );
    }

    #[test]
    fn parse_decimal_rejects_empty() {
        assert_eq!(parse_decimal_u64_from_ascii_bytes(b""), None);
    }

    #[test]
    fn parse_decimal_rejects_non_digit() {
        assert_eq!(parse_decimal_u64_from_ascii_bytes(b"12a4"), None);
    }

    #[test]
    fn parse_decimal_rejects_leading_space() {
        assert_eq!(parse_decimal_u64_from_ascii_bytes(b" 123"), None);
    }

    #[test]
    fn parse_decimal_rejects_leading_plus() {
        assert_eq!(parse_decimal_u64_from_ascii_bytes(b"+123"), None);
    }

    #[test]
    fn parse_decimal_rejects_overflow_past_u64_max() {
        // u64::MAX is 18446744073709551615; one more is overflow.
        assert_eq!(
            parse_decimal_u64_from_ascii_bytes(b"18446744073709551616"),
            None
        );
    }

    #[test]
    fn parse_decimal_rejects_far_overflow() {
        assert_eq!(
            parse_decimal_u64_from_ascii_bytes(b"99999999999999999999"),
            None
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // Tests for read_memo_config_file
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn config_reader_extracts_text_message() {
        let fixture =
            FixtureFile::create("config_extracts", "text_message = \"plays_white:alice\"\n");
        let result = read_memo_config_file(fixture.path_as_str());
        eprintln!("DEBUG result = {:?}", result);
        eprintln!("DEBUG path = {}", fixture.path_as_str());
        let contents = result
            .expect("test: read must succeed")
            .expect("test: text_message must be present");
        assert_eq!(contents.text_message_as_bytes(), b"plays_white:alice");
        assert_eq!(contents.text_message_length, 17);
    }

    #[test]
    fn config_reader_returns_none_when_field_missing() {
        let fixture = FixtureFile::create(
            "config_missing_field",
            "owner = \"alice\"\nupdated_at_timestamp = \"1000\"\n",
        );
        let result = read_memo_config_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn config_reader_returns_none_when_value_too_long() {
        // Construct a value longer than MAX_CONFIG_TEXT_MESSAGE_BYTES (32).
        let oversize_value = "a".repeat(MAX_CONFIG_TEXT_MESSAGE_BYTES + 5);
        let toml_contents = format!("text_message = \"{}\"\n", oversize_value);
        let fixture = FixtureFile::create("config_too_long", &toml_contents);
        let result = read_memo_config_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn config_reader_accepts_other_fields_present() {
        // The file has multiple fields; the reader should pick out only
        // text_message and ignore the rest.
        let fixture = FixtureFile::create(
            "config_other_fields",
            "owner = \"alice\"\n\
             text_message = \"refresh_rate:10\"\n\
             updated_at_timestamp = \"1000\"\n",
        );
        let result = read_memo_config_file(fixture.path_as_str());
        eprintln!("DEBUG result = {:?}", result);
        eprintln!("DEBUG path = {}", fixture.path_as_str());
        let contents = result
            .expect("test: read must succeed")
            .expect("test: text_message must be present");
        assert_eq!(contents.text_message_as_bytes(), b"refresh_rate:10");
    }

    #[test]
    fn config_reader_rejects_empty_path() {
        let result = read_memo_config_file("");
        assert_eq!(result, Err(MemoConfigReadError::EmptyFilePath));
    }

    #[test]
    fn config_reader_reports_file_open_failed_for_nonexistent_file() {
        let mut nonexistent_path = std::env::temp_dir();
        nonexistent_path.push("memo_reader_test_definitely_does_not_exist.toml");
        // Make sure it really does not exist (best-effort cleanup of any
        // leftover from a previous failed test).
        let _ = std::fs::remove_file(&nonexistent_path);

        let path_str = nonexistent_path
            .to_str()
            .expect("test: temp path must be UTF-8");
        let result = read_memo_config_file(path_str);
        assert_eq!(result, Err(MemoConfigReadError::FileOpenFailed));
    }

    // ─────────────────────────────────────────────────────────────────
    // Tests for read_memo_move_file
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn move_reader_extracts_all_three_fields() {
        let fixture = FixtureFile::create(
            "move_all_three",
            "owner = \"alice\"\n\
             text_message = \"Nf3\"\n\
             updated_at_timestamp = \"1778532301\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        let contents = result
            .expect("test: read must succeed")
            .expect("test: all three fields must be present");
        assert_eq!(contents.owner_as_bytes(), b"alice");
        assert_eq!(contents.text_message_as_bytes(), b"Nf3");
        assert_eq!(contents.updated_at_unix_timestamp, 1_778_532_301);
    }

    #[test]
    fn move_reader_extracts_resign_command() {
        let fixture = FixtureFile::create(
            "move_resign",
            "owner = \"bob\"\n\
             text_message = \"resign\"\n\
             updated_at_timestamp = \"2000\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        let contents = result
            .expect("test: read must succeed")
            .expect("test: all three fields must be present");
        assert_eq!(contents.owner_as_bytes(), b"bob");
        assert_eq!(contents.text_message_as_bytes(), b"resign");
        assert_eq!(contents.updated_at_unix_timestamp, 2000);
    }

    #[test]
    fn move_reader_returns_none_when_owner_missing() {
        let fixture = FixtureFile::create(
            "move_no_owner",
            "text_message = \"Nf3\"\n\
             updated_at_timestamp = \"1000\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_text_message_missing() {
        let fixture = FixtureFile::create(
            "move_no_text",
            "owner = \"alice\"\n\
             updated_at_timestamp = \"1000\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_timestamp_missing() {
        let fixture = FixtureFile::create(
            "move_no_ts",
            "owner = \"alice\"\n\
             text_message = \"Nf3\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_timestamp_malformed() {
        let fixture = FixtureFile::create(
            "move_bad_ts",
            "owner = \"alice\"\n\
             text_message = \"Nf3\"\n\
             updated_at_timestamp = \"not_a_number\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_timestamp_overflows() {
        let fixture = FixtureFile::create(
            "move_overflow_ts",
            "owner = \"alice\"\n\
             text_message = \"Nf3\"\n\
             updated_at_timestamp = \"99999999999999999999\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_owner_empty_string() {
        let fixture = FixtureFile::create(
            "move_empty_owner",
            "owner = \"\"\n\
             text_message = \"Nf3\"\n\
             updated_at_timestamp = \"1000\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_text_message_empty_string() {
        let fixture = FixtureFile::create(
            "move_empty_text",
            "owner = \"alice\"\n\
             text_message = \"\"\n\
             updated_at_timestamp = \"1000\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_owner_too_long() {
        let oversize_owner = "a".repeat(MAX_USERNAME_BYTES + 2);
        let toml_contents = format!(
            "owner = \"{}\"\n\
             text_message = \"Nf3\"\n\
             updated_at_timestamp = \"1000\"\n",
            oversize_owner
        );
        let fixture = FixtureFile::create("move_owner_too_long", &toml_contents);
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_returns_none_when_text_message_too_long() {
        let oversize_text = "a".repeat(MAX_MOVE_TEXT_MESSAGE_BYTES + 2);
        let toml_contents = format!(
            "owner = \"alice\"\n\
             text_message = \"{}\"\n\
             updated_at_timestamp = \"1000\"\n",
            oversize_text
        );
        let fixture = FixtureFile::create("move_text_too_long", &toml_contents);
        let result = read_memo_move_file(fixture.path_as_str());
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn move_reader_rejects_empty_path() {
        let result = read_memo_move_file("");
        assert_eq!(result, Err(MemoMoveReadError::EmptyFilePath));
    }

    #[test]
    fn move_reader_reports_file_open_failed_for_nonexistent_file() {
        let mut nonexistent_path = std::env::temp_dir();
        nonexistent_path.push("memo_reader_test_move_definitely_does_not_exist.toml");
        let _ = std::fs::remove_file(&nonexistent_path);

        let path_str = nonexistent_path
            .to_str()
            .expect("test: temp path must be UTF-8");
        let result = read_memo_move_file(path_str);
        assert_eq!(result, Err(MemoMoveReadError::FileOpenFailed));
    }

    #[test]
    fn move_reader_handles_extra_unknown_fields() {
        // Files may contain other fields. The reader must ignore them.
        let fixture = FixtureFile::create(
            "move_extra_fields",
            "owner = \"alice\"\n\
             text_message = \"e4\"\n\
             updated_at_timestamp = \"1000\"\n\
             extra_field = \"ignored\"\n\
             another_extra = \"also ignored\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        let contents = result
            .expect("test: read must succeed")
            .expect("test: all required fields must be present");
        assert_eq!(contents.owner_as_bytes(), b"alice");
        assert_eq!(contents.text_message_as_bytes(), b"e4");
        assert_eq!(contents.updated_at_unix_timestamp, 1000);
    }

    #[test]
    fn move_reader_accepts_zero_timestamp() {
        // Unusual but not malformed. The reader should accept; semantic
        // policy on timestamps belongs to the caller, not the reader.
        let fixture = FixtureFile::create(
            "move_zero_ts",
            "owner = \"alice\"\n\
             text_message = \"e4\"\n\
             updated_at_timestamp = \"0\"\n",
        );
        let result = read_memo_move_file(fixture.path_as_str());
        let contents = result
            .expect("test: read must succeed")
            .expect("test: zero timestamp is a valid u64");
        assert_eq!(contents.updated_at_unix_timestamp, 0);
    }

    // ─────────────────────────────────────────────────────────────────
    // Copy semantics
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn memo_structs_are_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<MemoConfigFileContents>();
        assert_copy::<MemoMoveFileContents>();
        assert_copy::<MemoConfigReadError>();
        assert_copy::<MemoMoveReadError>();
    }
}
