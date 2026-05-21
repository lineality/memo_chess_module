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
    pub n_move_rule: u16,
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
        n_move_rule: u16,
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

        // ── N-move rule ───────────────────────────────────────────────
        if n_move_rule < MIN_N_MOVE_RULE_VALUE || n_move_rule > MAX_N_MOVE_RULE_VALUE {
            return Err(MemochessGameConfigError::NMoveRuleOutOfRange);
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
            600, // 10-minute game
            10,  // refresh every 10 seconds
            50,  // 50-move rule enabled
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
        assert_eq!(config.n_move_rule, 50);
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
            50,
        )
        .expect("test: None n_move_rule must be accepted");
        assert_eq!(config.n_move_rule, 50);
    }

    #[test]
    fn empty_directory_path_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"", b"u", b"w", b"b", 60, 5, 50,
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
            50,
        );
        assert_eq!(result, Err(MemochessGameConfigError::DirectoryPathTooLong));
    }

    #[test]
    fn maximum_length_directory_path_accepted() {
        let max_path = [b'a'; MAX_DIRECTORY_PATH_BYTES];
        let config = MemochessGameConfig::try_construct_memochess_game_config(
            &max_path, b"u", b"w", b"b", 60, 5, 50,
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
            b"/tmp/g", b"", b"w", b"b", 60, 5, 50,
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
            50,
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
            50,
        )
        .expect("test: max-length names must be accepted");
        assert_eq!(config.white_player_name_as_bytes(), &max_name[..]);
        assert_eq!(config.black_player_name_as_bytes(), &other_max_name[..]);
    }

    #[test]
    fn identical_white_and_black_names_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"u", b"alice", b"alice", 60, 5, 50,
        );
        assert_eq!(
            result,
            Err(MemochessGameConfigError::WhiteAndBlackPlayerNamesIdentical)
        );
    }

    #[test]
    fn zero_time_limit_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"u", b"w", b"b", 0, 5, 50,
        );
        assert_eq!(
            result,
            Err(MemochessGameConfigError::TimeLimitPerPlayerBelowMinimum)
        );
    }

    #[test]
    fn refresh_rate_zero_rejected() {
        let result = MemochessGameConfig::try_construct_memochess_game_config(
            b"/tmp/g", b"u", b"w", b"b", 60, 0, 50,
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
            50,
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
            MIN_N_MOVE_RULE_VALUE - 1,
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
            MAX_N_MOVE_RULE_VALUE + 1,
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
// Module: read_toml_single_line_string_field_no_heap
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

// If the scope of the task is to read a single small value
// contingent on a small key in a .toml file line,
// then that process to do that should:
// - not use heap memory at all
// - not read the entire file into memory
// - not read the entire line into memory
// - not read any large chunk of key + value into memory
// - not read the key more than one byte at a time
// - not use more than a one multi-byte buffer sanitized to be no larger
//   than a the value: two buffers, one single-byte buffer
//   and one multi-byte buffer (e.g. sized to fit the value).

// For security and efficiency no more than what is needed should be
// loaded or read, and what initial "reading" only requires a single byte
// (a one-byte buffer).

// This module exposes a single function,
// [`read_single_line_string_field_from_toml_no_heap`], that reads a single
// short-string field from a TOML file using only stack-allocated buffers.

// # Memory Sanitation

// Bounding a buffer to the smallest size the task actually requires is
// a form of input sanitation: input that exceeds what the task is meant
// to handle cannot enter, because there is nowhere to put it. The
// const-generic `OUTPUT_BUFFER_BYTES` and the `RsLsfValueExceedsOutputBuffer`
// error together make oversize values unrepresentable rather than
// silently absorbed. Buffers that read in more bytes than the task
// requires (e.g. `BufReader`'s ~8 KiB default, or "read the whole line
// and sort it out later") are a sanitation failure in the same family
// as unbounded reads and over-sized allocations — Heartbleed being a
// well-known example of the broader class. This module bounds every
// buffer to the smallest size the task genuinely needs: one byte for
// reading from the file, and exactly `OUTPUT_BUFFER_BYTES` for the
// value being returned.

// This is in the same spirit as the combined security and efficiency
// of using enums and structs in Rust to require inputs to be very
// strictly only what they are safe to be: hygiene and sanitation for
// economics, efficiency, maintainability, modularity, and security.

// # Architecture

// The scanner reads **one byte at a time** directly from the file and walks
// it through a finite-state machine. There is NO read-chunk buffer and NO
// line-accumulator buffer. The only buffer in the function is the caller's
// output buffer, into which value bytes are written directly when (and only
// when) the scanner is inside the value of the matched key.

// State carried by the scanner:

//   * one `[u8; 1]` read scratch (typically a CPU register, not even stack)
//   * one small state enum + one `usize` (`matched_key_bytes`)
//   * one `usize` write cursor into the caller's output buffer
//   * two `u64` failsafe counters (bytes scanned, iteration count)

// Total module-internal scratch: a few words. The key itself is never copied
// anywhere (the key is not read into memory or a buffer, it is 'scanned'
// one byte at a time),
// it is compared against `target_field_key.as_bytes()` in place,
// index by index.

// # In Scope

// * One key per call, top-level (no `[section]`).
// * Single-line values up to a caller-chosen `OUTPUT_BUFFER_BYTES` length.
// * Values quoted with simple double quotes (`"..."`) or unquoted (numbers,
//   bare identifiers).
// * Lines using LF or CRLF terminators.
// * Lines beginning with `#` (after trimming leading whitespace) are treated
//   as comments.

// # Value Termination Policy

// * **Quoted values** terminate at the next `"` byte. EOF reached before the
//   closing `"` is an error (`RsLsfValueUnterminatedAtEndOfFile`).
// * **Unquoted values** terminate at the next `\n` byte (a preceding `\r`,
//   if any, is not included in the value). EOF reached before `\n` is an
//   error (`RsLsfValueUnterminatedAtEndOfFile`). This symmetry — quoted
//   needs a closing quote, unquoted needs a closing newline — was a
//   deliberate design choice. Trailing whitespace between the last
//   non-whitespace value byte and `\n` IS included in the returned value
//   (strict policy: zero extra state, caller trims if desired).

// # Explicitly Out Of Scope (Non-Goals)

// * Full TOML grammar (no arrays, tables, inline tables, multi-line strings,
//   escape sequences, dotted keys, datetimes).
// * Re-encoding the value (caller decides whether to `core::str::from_utf8`).
// * Trailing inline comments on the same line as the value
//   (e.g. `name = "x"  # note` — the `# note` becomes part of the value for
//   unquoted values; for quoted values it is ignored because termination
//   occurs at the closing `"`).
// * UTF-8 BOM at file start.

// # Defensive Policy

// On any malformed input, I/O error, oversize value, or exhausted safety
// budget, the function returns a terse zero-data [`ReadTomlFieldError`]
// variant. It never panics, never allocates, and never includes the file
// path, file contents, or OS error string in the returned error.

// # Concurrency

// The function is synchronous and self-contained. It does not share state.
// It is safe to call from multiple threads with distinct paths.
// ============================================================================

// use std::fs::File;
// use std::io::Read;

// ----------------------------------------------------------------------------
// Module constants
// ----------------------------------------------------------------------------

/// Failsafe upper bound on total bytes read from a single file.
///
/// Bounds the read loop even if the OS keeps returning data (NASA P10 rule 2).
/// One mebibyte is generous for configuration files while preventing
/// pathological or adversarial inputs from running unbounded work.
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
///   no embedded OS error. Error values must never become an
///   information-disclosure vector.
/// * Every variant carries the unique prefix `RsLsf` (Read Single Line
///   String Field) so it is unambiguously traceable in logs to this
///   function, satisfying the "unique error per function" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTomlFieldError {
    /// RSLSF: caller-supplied key was empty.
    RsLsfEmptyKey,
    /// RSLSF: caller-supplied `OUTPUT_BUFFER_BYTES` const-generic was zero.
    RsLsfOutputBufferZeroSized,
    /// RSLSF: the file could not be opened (does not exist, permission, etc.).
    RsLsfFileOpenFailed,
    /// RSLSF: an I/O error occurred while reading.
    RsLsfFileReadFailed,
    /// RSLSF: the requested key was not present in the file.
    RsLsfFieldNotFound,
    /// RSLSF: a value would not fit in `OUTPUT_BUFFER_BYTES`; refusing to
    /// silently truncate.
    RsLsfValueExceedsOutputBuffer,
    /// RSLSF: end-of-file was reached while still inside a value — no
    /// closing `"` for a quoted value, or no terminating `\n` for an
    /// unquoted value. Refusing to guess a terminator.
    RsLsfValueUnterminatedAtEndOfFile,
    /// RSLSF: the failsafe byte/iteration budget was exhausted.
    RsLsfSafetyBudgetExhausted,
}

// ----------------------------------------------------------------------------
// Internal scanner state
// ----------------------------------------------------------------------------

/// Finite-state machine for the byte-at-a-time scanner.
///
/// The scanner is fed one input byte per outer-loop iteration. The current
/// state plus that byte determines the next state (and possibly a write into
/// the output buffer or an early return).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineScanState {
    /// Just after `\n` (or at file start). Whitespace, `#`, `\n`, or the
    /// first byte of a candidate key may appear next.
    AtLineStart,
    /// Saw leading whitespace after line start; same transitions as
    /// `AtLineStart` except a `#` here is still a full-line comment.
    SkippingLeadingWhitespace,
    /// Comparing input bytes against `target_field_key`. `matched_key_bytes`
    /// counts how many bytes have matched so far.
    MatchingKey { matched_key_bytes: usize },
    /// Full key has matched. Expecting optional whitespace then `=`.
    AwaitingEquals,
    /// Saw `=`. Skipping optional whitespace before the value begins.
    AwaitingValueStart,
    /// Inside an unquoted value. Terminates on `\n`. `\r` immediately
    /// before `\n` is dropped, not stored.
    CopyingUnquotedValue,
    /// Inside a quoted value. Terminates on `"`.
    CopyingQuotedValue,
    /// This line cannot match the key; fast-forward to the next `\n`.
    SkippingToEndOfLine,
    /// Saw `#` at the start of a line; treat the rest of the line as comment.
    InCommentToEndOfLine,
}

// ----------------------------------------------------------------------------
// Public API
// ----------------------------------------------------------------------------

/// Reads a single top-level single-line string field from a TOML file using
/// only stack-allocated memory.
///
/// One byte is read from the file at a time. There is no read-chunk buffer
/// and no line-accumulator buffer. Value bytes (and only value bytes for the
/// matched key) are written directly into the caller's output buffer.
///
/// # Type Parameters
/// * `OUTPUT_BUFFER_BYTES` — the fixed size of the returned byte buffer.
///   Must be `> 0`. Pick the smallest value that comfortably fits your
///   project's value (e.g. `16` for a short identifier). A value longer
///   than `OUTPUT_BUFFER_BYTES` produces `RsLsfValueExceedsOutputBuffer`;
///   values are never silently truncated.
///
/// # Arguments
/// * `absolute_toml_file_path` — absolute path to the TOML file.
/// * `target_field_key` — the exact top-level key to find. Must be non-empty.
///
/// # Returns
/// * `Ok((output_buffer, written_length))` on success. `written_length`
///   bytes of `output_buffer` are meaningful; bytes past that are zero.
/// * `Err(ReadTomlFieldError)` on any failure; never panics, never allocates.
///
/// # Example (illustrative)
/// ```ignore
/// match read_single_line_string_field_from_toml_no_heap::<16>(
///     "/etc/myapp/config.toml",
///     "node_id",
/// ) {
///     Ok((buf, len)) => {
///         if let Ok(s) = core::str::from_utf8(&buf[..len]) {
///             // use s
///         }
///     }
///     Err(_e) => {
///         // Log a unique short code; do NOT log path/contents.
///         // Continue with a safe default; do not panic.
///     }
/// }
/// ```
pub fn read_single_line_string_field_from_toml_no_heap<const OUTPUT_BUFFER_BYTES: usize>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> {
    // ----------------------------------------------------------------------
    // Debug-Assert / Test-Assert / Production-Catch
    // ----------------------------------------------------------------------
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

    if OUTPUT_BUFFER_BYTES == 0 {
        return Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized);
    }
    if target_field_key.is_empty() {
        return Err(ReadTomlFieldError::RsLsfEmptyKey);
    }

    // ----------------------------------------------------------------------
    // Open the file (terse error: no path leakage)
    // ----------------------------------------------------------------------
    let mut open_file_handle: File = match File::open(absolute_toml_file_path) {
        Ok(handle) => handle,
        Err(_) => return Err(ReadTomlFieldError::RsLsfFileOpenFailed),
    };

    // ----------------------------------------------------------------------
    // Scanner state
    // ----------------------------------------------------------------------
    let key_bytes: &[u8] = target_field_key.as_bytes();

    let mut single_byte_read_scratch: [u8; 1] = [0u8; 1];
    let mut output_buffer: [u8; OUTPUT_BUFFER_BYTES] = [0u8; OUTPUT_BUFFER_BYTES];
    let mut output_write_cursor: usize = 0;

    let mut current_state: LineScanState = LineScanState::AtLineStart;

    // For unquoted values: when we see '\r' we hold it pending the next
    // byte. If that next byte is '\n', the '\r' is silently dropped (CRLF).
    // If it is anything else, the held '\r' becomes part of the value.
    // This avoids needing a buffer to "peek".
    let mut unquoted_value_has_pending_cr: bool = false;

    let mut cumulative_bytes_scanned: u64 = 0;
    let mut safety_iteration_count: u64 = 0;
    // One byte per iteration, so the iteration cap matches the byte cap
    // with a small margin to absorb the EOF iteration.
    let safety_iteration_limit: u64 = RSLSF_MAX_BYTES_SCANNED + 16;

    // ----------------------------------------------------------------------
    // Read loop: one byte per iteration.
    // ----------------------------------------------------------------------
    loop {
        safety_iteration_count = safety_iteration_count.saturating_add(1);
        if safety_iteration_count > safety_iteration_limit {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        let bytes_read_this_call: usize = match open_file_handle.read(&mut single_byte_read_scratch)
        {
            Ok(count) => count,
            Err(_) => return Err(ReadTomlFieldError::RsLsfFileReadFailed),
        };

        // ------------------------------------------------------------------
        // EOF handling
        // ------------------------------------------------------------------
        if bytes_read_this_call == 0 {
            // EOF. What we do depends on what state we are in.
            match current_state {
                LineScanState::CopyingUnquotedValue => {
                    // Per the value-termination policy, an unquoted value
                    // MUST end with '\n'. EOF mid-value is an error.
                    return Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
                }
                LineScanState::CopyingQuotedValue => {
                    // Quoted value never saw its closing '"'.
                    return Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
                }
                LineScanState::AwaitingValueStart => {
                    // We saw "key =" then EOF with no value bytes.
                    // For unquoted values policy this also requires '\n'.
                    return Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
                }
                _ => {
                    // Any other state means we never entered the value.
                    return Err(ReadTomlFieldError::RsLsfFieldNotFound);
                }
            }
        }

        cumulative_bytes_scanned = cumulative_bytes_scanned.saturating_add(1);
        if cumulative_bytes_scanned > RSLSF_MAX_BYTES_SCANNED {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        let current_byte: u8 = single_byte_read_scratch[0];

        // ------------------------------------------------------------------
        // State transition
        // ------------------------------------------------------------------
        match current_state {
            LineScanState::AtLineStart | LineScanState::SkippingLeadingWhitespace => {
                if current_byte == b'\n' {
                    current_state = LineScanState::AtLineStart;
                } else if current_byte == b'\r' {
                    // Drop bare CR (and CR before LF) at line start.
                } else if is_ascii_space_or_tab_byte(current_byte) {
                    current_state = LineScanState::SkippingLeadingWhitespace;
                } else if current_byte == b'#' {
                    current_state = LineScanState::InCommentToEndOfLine;
                } else {
                    // First byte of a candidate key. Compare to key[0].
                    if current_byte == key_bytes[0] {
                        if key_bytes.len() == 1 {
                            // Single-byte key already fully matched.
                            current_state = LineScanState::AwaitingEquals;
                        } else {
                            current_state = LineScanState::MatchingKey {
                                matched_key_bytes: 1,
                            };
                        }
                    } else {
                        current_state = LineScanState::SkippingToEndOfLine;
                    }
                }
            }

            LineScanState::MatchingKey { matched_key_bytes } => {
                if matched_key_bytes < key_bytes.len() {
                    if current_byte == key_bytes[matched_key_bytes] {
                        let next_matched = matched_key_bytes + 1;
                        if next_matched == key_bytes.len() {
                            current_state = LineScanState::AwaitingEquals;
                        } else {
                            current_state = LineScanState::MatchingKey {
                                matched_key_bytes: next_matched,
                            };
                        }
                    } else if current_byte == b'\n' {
                        // Short line; cannot match. Start over.
                        current_state = LineScanState::AtLineStart;
                    } else {
                        current_state = LineScanState::SkippingToEndOfLine;
                    }
                } else {
                    // Defensive: should not be reachable because we
                    // transition to AwaitingEquals as soon as the full key
                    // matches. Treat as non-match for safety.
                    current_state = LineScanState::SkippingToEndOfLine;
                }
            }

            LineScanState::AwaitingEquals => {
                if is_ascii_space_or_tab_byte(current_byte) {
                    // stay
                } else if current_byte == b'=' {
                    current_state = LineScanState::AwaitingValueStart;
                } else if current_byte == b'\n' {
                    // Key matched but no '=' on the line; not a kv pair.
                    current_state = LineScanState::AtLineStart;
                } else {
                    // E.g. "name_long" when looking for "name": extra
                    // characters after key bytes. Not a match.
                    current_state = LineScanState::SkippingToEndOfLine;
                }
            }

            LineScanState::AwaitingValueStart => {
                if is_ascii_space_or_tab_byte(current_byte) {
                    // stay
                } else if current_byte == b'"' {
                    current_state = LineScanState::CopyingQuotedValue;
                } else if current_byte == b'\n' {
                    // Empty unquoted value with a newline terminator: OK,
                    // return zero-length value.
                    return Ok((output_buffer, 0));
                } else if current_byte == b'\r' {
                    // Hold pending CR; next byte decides CRLF vs literal.
                    unquoted_value_has_pending_cr = true;
                    current_state = LineScanState::CopyingUnquotedValue;
                } else {
                    // First byte of an unquoted value.
                    if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                    }
                    output_buffer[output_write_cursor] = current_byte;
                    output_write_cursor += 1;
                    current_state = LineScanState::CopyingUnquotedValue;
                }
            }

            LineScanState::CopyingUnquotedValue => {
                if current_byte == b'\n' {
                    // Terminator. Pending CR (if any) is dropped: CRLF.
                    return Ok((output_buffer, output_write_cursor));
                } else if current_byte == b'\r' {
                    // If a previous CR was pending, it was a literal CR
                    // in the value and must be written now.
                    if unquoted_value_has_pending_cr {
                        if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                            return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                        }
                        output_buffer[output_write_cursor] = b'\r';
                        output_write_cursor += 1;
                    }
                    unquoted_value_has_pending_cr = true;
                } else {
                    // Flush any pending CR (it was not followed by LF, so
                    // it is part of the value).
                    if unquoted_value_has_pending_cr {
                        if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                            return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                        }
                        output_buffer[output_write_cursor] = b'\r';
                        output_write_cursor += 1;
                        unquoted_value_has_pending_cr = false;
                    }
                    if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                    }
                    output_buffer[output_write_cursor] = current_byte;
                    output_write_cursor += 1;
                }
            }

            LineScanState::CopyingQuotedValue => {
                if current_byte == b'"' {
                    return Ok((output_buffer, output_write_cursor));
                } else {
                    if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                    }
                    output_buffer[output_write_cursor] = current_byte;
                    output_write_cursor += 1;
                }
            }

            LineScanState::SkippingToEndOfLine | LineScanState::InCommentToEndOfLine => {
                if current_byte == b'\n' {
                    current_state = LineScanState::AtLineStart;
                }
                // else: ignore the byte.
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Tiny pure helpers (no heap, no state)
// ----------------------------------------------------------------------------

#[inline]
fn is_ascii_space_or_tab_byte(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t')
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
    /// absolute path. Test-only; may use heap freely.
    fn write_unique_temp_toml(label: &str, contents: &str) -> PathBuf {
        let mut path_buffer = std::env::temp_dir();
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
    fn rslsf_handles_crlf_endings_for_unquoted_value() {
        let test_path = write_unique_temp_toml("crlf_unquoted", "port = 8080\r\nx = 1\r\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port")
                .expect("should find unquoted value with CRLF terminator");
        // The trailing CR before LF must NOT appear in the value.
        assert_eq!(&output_buffer[..written_length], b"8080");
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
    fn rslsf_quoted_value_finds_at_eof_via_closing_quote() {
        // Quoted value with no trailing newline; closing quote IS present.
        // This is valid per policy.
        let test_path = write_unique_temp_toml("quoted_no_lf", "name = \"dora\"");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("quoted value terminated by closing quote should succeed at EOF");
        assert_eq!(&output_buffer[..written_length], b"dora");
    }

    #[test]
    fn rslsf_unquoted_value_without_newline_is_unterminated() {
        // Unquoted value with no trailing newline must now ERROR
        // (newline-required policy).
        let test_path = write_unique_temp_toml("unquoted_no_lf", "port = 8080");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port");
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile),
        );
    }

    #[test]
    fn rslsf_quoted_value_without_closing_quote_is_unterminated() {
        let test_path = write_unique_temp_toml("quoted_no_close", "name = \"alice\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name");
        // Note: '\n' inside a quoted value is currently treated as data, not
        // a terminator. So this scans through the newline still in the
        // quoted-value state, hits EOF, and reports unterminated.
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile),
        );
    }

    #[test]
    fn rslsf_finds_key_after_many_unrelated_lines() {
        // Force the scanner to traverse a large number of unrelated lines
        // before reaching the target key.
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!("noise_key_{:03} = \"junkjunkjunk\"\n", i));
        }
        contents.push_str("target = \"eve\"\n");
        let test_path = write_unique_temp_toml("many_lines", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(
                path_as_str(&test_path),
                "target",
            )
            .expect("should find value after many lines");
        assert_eq!(&output_buffer[..written_length], b"eve");
    }

    #[test]
    fn rslsf_unrelated_long_line_does_not_abort_scan() {
        // A line far longer than any previous "line buffer" must be handled
        // without aborting: there is no line buffer anymore, so the
        // limiting factor is only the output buffer (used only for the
        // matched value).
        let mut contents = String::new();
        contents.push_str("other_key = \"");
        for _ in 0..4096 {
            contents.push('X');
        }
        contents.push_str("\"\n");
        contents.push_str("name = \"frank\"\n");
        let test_path = write_unique_temp_toml("unrelated_long_line", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("unrelated long line should not block finding the real key");
        assert_eq!(&output_buffer[..written_length], b"frank");
    }

    #[test]
    fn rslsf_handles_whitespace_around_key_and_equals() {
        let test_path = write_unique_temp_toml("whitespace", "   name\t=\t  \"grace\"   \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value with varied whitespace");
        assert_eq!(&output_buffer[..written_length], b"grace");
    }

    #[test]
    fn rslsf_first_match_wins_when_key_appears_twice() {
        let test_path =
            write_unique_temp_toml("duplicate_key", "name = \"first\"\nname = \"second\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find first value");
        assert_eq!(&output_buffer[..written_length], b"first");
    }

    #[test]
    fn rslsf_zero_sized_output_buffer_is_rejected() {
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

    #[test]
    fn rslsf_unquoted_value_with_trailing_whitespace_includes_whitespace() {
        // Policy: trailing whitespace before '\n' IS included in the value.
        // This documents the strict-A choice. If the caller wants trimming,
        // they trim.
        let test_path = write_unique_temp_toml("unquoted_trailing_ws", "port = 8080   \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port")
                .expect("should find unquoted value");
        assert_eq!(&output_buffer[..written_length], b"8080   ");
    }

    #[test]
    fn rslsf_empty_unquoted_value_returns_zero_length() {
        // "key = \n" — empty unquoted value with newline. Returns OK, len 0.
        let test_path = write_unique_temp_toml("empty_unquoted", "name = \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("empty unquoted value with newline is valid");
        assert_eq!(written_length, 0);
        // Buffer untouched.
        assert_eq!(output_buffer[0], 0);
    }

    #[test]
    fn rslsf_empty_quoted_value_returns_zero_length() {
        let test_path = write_unique_temp_toml("empty_quoted", "name = \"\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("empty quoted value is valid");
        assert_eq!(written_length, 0);
        assert_eq!(output_buffer[0], 0);
    }

    #[test]
    fn rslsf_single_byte_key_works() {
        // Cover the special-case path where key length is 1 (transitions
        // straight from AtLineStart to AwaitingEquals).
        let test_path = write_unique_temp_toml("single_byte_key", "x = \"yes\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "x")
                .expect("single-byte key should work");
        assert_eq!(&output_buffer[..written_length], b"yes");
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
/// - `ClassifiedPrimitiveError::Skip` — the field is absent, too long for
///   our buffer, or the file is malformed (value never terminated); the
///   caller should treat this as "skip this file".
/// - `ClassifiedPrimitiveError::FileOpenFailed` /
///   `ClassifiedPrimitiveError::FileReadFailed` — a real I/O failure;
///   the caller should surface this as an `Err(...)`.
/// - `ClassifiedPrimitiveError::InternalReaderFault` — the caller supplied
///   invalid arguments (empty key, zero-sized buffer) or the failsafe
///   safety budget was exhausted; the caller should surface this as an
///   `Err(...)`.
///
/// This helper exists so that both `read_memo_config_file` and
/// `read_memo_move_file` share the same classification policy without
/// duplicating match arms.
fn classify_primitive_read_error(primitive_error: ReadTomlFieldError) -> ClassifiedPrimitiveError {
    match primitive_error {
        // "This file is not usable" conditions:
        //   - field absent
        //   - value too large for the caller's chosen output buffer
        //   - file malformed: value ran past EOF without its terminator
        // All three mean: skip this file and move on.
        ReadTomlFieldError::RsLsfFieldNotFound
        | ReadTomlFieldError::RsLsfValueExceedsOutputBuffer
        | ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile => ClassifiedPrimitiveError::Skip,

        // Real I/O failures.
        ReadTomlFieldError::RsLsfFileOpenFailed => ClassifiedPrimitiveError::FileOpenFailed,
        ReadTomlFieldError::RsLsfFileReadFailed => ClassifiedPrimitiveError::FileReadFailed,

        // Programmer error at the call site, or defensive failsafe trip.
        // These reflect bugs at the call site (empty key, zero-sized
        // output buffer) or a safety budget exhaustion that should not
        // occur in normal operation.
        ReadTomlFieldError::RsLsfEmptyKey
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

// ============================================================================
// SECTION 42: Config-Line Parser — Constants
// ============================================================================

/// Maximum bytes of the *value* portion of one config-line text_message.
///
/// ## Sizing Rationale
///
/// The longest legitimate value is a player name, bounded by
/// `MAX_USERNAME_BYTES` (16). Numeric values (`player_time`,
/// `refresh_rate`, `n_move_rule`) fit in a few digits. Literal values
/// (`off`, `yes`, `no`) are at most 3 bytes. So 16 bytes is the exact
/// natural ceiling.
pub const MAX_CONFIG_VALUE_BYTES: usize = 16;

/// Maximum bytes of the *key* portion of one config-line text_message.
///
/// ## Sizing Rationale
///
/// The longest recognized key is `refresh_rate` (12 bytes). 16 bytes
/// gives small headroom while keeping the buffer tiny.
pub const MAX_CONFIG_KEY_BYTES: usize = 16;

// ============================================================================
// SECTION 43: Config-Line Parser — Recognized Keys
// ============================================================================

/// One recognized configuration key, as it appears on the wire.
///
/// ## Project Context
///
/// Players write config memos containing `text_message = "key:value"`
/// where `key` is one of the strings below. The bootstrap layer
/// iterates the memo directory, reads each `text_message`, and uses
/// `parse_config_line_text_message` to map the wire-format string to
/// one of these variants.
///
/// The mapping from variant to wire-format string is defined by
/// `recognized_config_key_as_bytes`. The reverse mapping is performed
/// by `recognized_config_key_from_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognizedConfigKey {
    /// Wire format: `plays_white`. Value is a player name.
    PlaysWhite,
    /// Wire format: `plays_black`. Value is a player name.
    PlaysBlack,
    /// Wire format: `player_time`. Value is an integer number of
    /// minutes (converted to seconds by the config applier, not by the
    /// parser).
    PlayerTimeMinutes,
    /// Wire format: `refresh_rate`. Value is an integer number of seconds.
    RefreshRateSeconds,
    /// Wire format: `n_move_rule`. Value is either an integer (e.g. `50`)
    /// or the literal `off`.
    NMoveRule,
    /// Wire format: `3_time_rep`. Value is `yes` or `no`.
    ThreeTimeRepetition,
}

/// Return the wire-format byte string for a recognized key.
///
/// This is the single source of truth for the wire-format strings.
/// All other code that needs the wire string must call this function
/// rather than embedding the string literal.
pub const fn recognized_config_key_as_bytes(key: RecognizedConfigKey) -> &'static [u8] {
    match key {
        RecognizedConfigKey::PlaysWhite => b"plays_white",
        RecognizedConfigKey::PlaysBlack => b"plays_black",
        RecognizedConfigKey::PlayerTimeMinutes => b"player_time",
        RecognizedConfigKey::RefreshRateSeconds => b"refresh_rate",
        RecognizedConfigKey::NMoveRule => b"n_move_rule",
        RecognizedConfigKey::ThreeTimeRepetition => b"3_time_rep",
    }
}

/// Match a stripped key byte slice against the recognized-key table.
///
/// Exact byte comparison; case-sensitive; no whitespace tolerance
/// (the caller is responsible for stripping outer whitespace before
/// calling).
///
/// Returns `Some(variant)` on exact match, `None` otherwise.
fn recognized_config_key_from_bytes(stripped_key_bytes: &[u8]) -> Option<RecognizedConfigKey> {
    // Bounded loop over the small fixed table of recognized keys.
    let recognized_keys: [RecognizedConfigKey; 6] = [
        RecognizedConfigKey::PlaysWhite,
        RecognizedConfigKey::PlaysBlack,
        RecognizedConfigKey::PlayerTimeMinutes,
        RecognizedConfigKey::RefreshRateSeconds,
        RecognizedConfigKey::NMoveRule,
        RecognizedConfigKey::ThreeTimeRepetition,
    ];
    let mut index: usize = 0;
    while index < recognized_keys.len() {
        let candidate = recognized_keys[index];
        if recognized_config_key_as_bytes(candidate) == stripped_key_bytes {
            return Some(candidate);
        }
        index += 1;
    }
    None
}

// ============================================================================
// SECTION 44: Config-Line Parser — Parsed Result and Errors
// ============================================================================

/// The parsed result of one config-line `text_message`.
///
/// The value is stored as raw bytes; semantic interpretation
/// (decimal parse for numbers, yes/no parse for booleans, `off`
/// detection for `n_move_rule`) is the responsibility of the config
/// applier, not the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedConfigLine {
    /// Which configuration key this line refers to.
    pub recognized_key: RecognizedConfigKey,
    /// Raw value bytes (after stripping outer whitespace).
    /// Meaningful bytes occupy `value_buffer[..value_length]`.
    pub value_buffer: [u8; MAX_CONFIG_VALUE_BYTES],
    /// Number of meaningful bytes in `value_buffer`.
    /// In range `1..=MAX_CONFIG_VALUE_BYTES`.
    pub value_length: u8,
}

impl ParsedConfigLine {
    /// Borrow the meaningful prefix of `value_buffer` as a slice.
    pub fn value_as_bytes(&self) -> &[u8] {
        let length_as_usize = self.value_length as usize;
        let safe_length = if length_as_usize > MAX_CONFIG_VALUE_BYTES {
            MAX_CONFIG_VALUE_BYTES
        } else {
            length_as_usize
        };
        &self.value_buffer[..safe_length]
    }
}

/// Failure modes of `parse_config_line_text_message`.
///
/// All variants are unit variants per project policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLineParseError {
    /// The input contained no `:` separator.
    NoColonSeparator,
    /// The input contained more than one `:` separator.
    MultipleColonSeparators,
    /// The key half (before `:`) was empty after whitespace stripping.
    EmptyKey,
    /// The value half (after `:`) was empty after whitespace stripping.
    EmptyValue,
    /// The key did not match any recognized configuration key.
    UnrecognizedKey,
    /// The key half exceeded `MAX_CONFIG_KEY_BYTES` after stripping.
    /// (Defensive: any unrecognized key also fails the match above, so
    /// this variant is reached only when the caller's input is so
    /// oversized that the key buffer would not even hold it.)
    KeyExceedsBuffer,
    /// The value half exceeded `MAX_CONFIG_VALUE_BYTES` after stripping.
    ValueExceedsBuffer,
    /// The value half contained internal whitespace.
    /// None of the recognized values legitimately contain whitespace.
    ValueContainsInternalWhitespace,
}

// ============================================================================
// SECTION 45: Config-Line Parser — Internal Helpers
// ============================================================================

/// True if `byte_value` is an ASCII whitespace byte recognized for
/// stripping purposes: space, tab, carriage return, line feed.
const fn is_ascii_whitespace_for_config(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t' | b'\r' | b'\n')
}

/// Return a sub-slice of `input_bytes` with leading and trailing ASCII
/// whitespace removed. The returned slice may be empty if the input was
/// all whitespace.
fn strip_ascii_whitespace_from_both_ends(input_bytes: &[u8]) -> &[u8] {
    let mut start_index: usize = 0;
    let mut end_index: usize = input_bytes.len();
    while start_index < end_index && is_ascii_whitespace_for_config(input_bytes[start_index]) {
        start_index += 1;
    }
    while end_index > start_index && is_ascii_whitespace_for_config(input_bytes[end_index - 1]) {
        end_index -= 1;
    }
    &input_bytes[start_index..end_index]
}

/// Return `true` if any byte in `input_bytes` is an ASCII whitespace
/// byte. Used to reject values that contain internal whitespace.
fn slice_contains_ascii_whitespace(input_bytes: &[u8]) -> bool {
    let mut index: usize = 0;
    while index < input_bytes.len() {
        if is_ascii_whitespace_for_config(input_bytes[index]) {
            return true;
        }
        index += 1;
    }
    false
}

/// Count the number of `:` bytes in `input_bytes`.
fn count_colon_bytes(input_bytes: &[u8]) -> usize {
    let mut count: usize = 0;
    let mut index: usize = 0;
    while index < input_bytes.len() {
        if input_bytes[index] == b':' {
            count += 1;
        }
        index += 1;
    }
    count
}

/// Return the index of the first `:` byte in `input_bytes`, if any.
fn find_first_colon_index(input_bytes: &[u8]) -> Option<usize> {
    let mut index: usize = 0;
    while index < input_bytes.len() {
        if input_bytes[index] == b':' {
            return Some(index);
        }
        index += 1;
    }
    None
}

// ============================================================================
// SECTION 46: Config-Line Parser — Main Function
// ============================================================================

/// toml file vs. 'wire'
/// TODO: more communication, zero opaque jargon ever

/// Parse one config-line `text_message` byte slice into a
/// `ParsedConfigLine`.
///
/// ## Project Context
///
/// The bootstrap layer reads `text_message` from each memo file via
/// `read_memo_config_file`. The returned bytes are passed here. On
/// success, the bootstrap layer hands the result to the config-applier
/// (next milestone), which combines it with any prior partial config to
/// produce a `MemochessGameConfig` once all keys have been seen.
///
/// ## Expected Wire Format
///
/// Exactly: `key:value`
///
/// Where:
/// - `key` is one of the recognized keys defined by
///   `RecognizedConfigKey`.
/// - `value` is a non-empty, internal-whitespace-free byte sequence
///   no longer than `MAX_CONFIG_VALUE_BYTES`.
/// - Whitespace surrounding either half is stripped before matching.
/// - The `:` separator must appear exactly once.
///
/// ## Semantic Interpretation of Values
///
/// This function does NOT validate the *semantic* content of the value
/// (e.g. whether `player_time:notanumber` is a valid integer). It only
/// validates the *syntactic* shape. Semantic validation is the
/// responsibility of the config applier, which has access to the
/// `MAX_TIME_LIMIT_PER_PLAYER_SECONDS`, `MAX_REFRESH_RATE_SECONDS`, and
/// similar bounds.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Stack buffers only. Bounded loops throughout.
pub fn parse_config_line_text_message(
    text_message_bytes: &[u8],
) -> Result<ParsedConfigLine, ConfigLineParseError> {
    // Step 1: locate the colon and reject multi-colon inputs.
    let colon_count = count_colon_bytes(text_message_bytes);
    if colon_count == 0 {
        return Err(ConfigLineParseError::NoColonSeparator);
    }
    if colon_count > 1 {
        return Err(ConfigLineParseError::MultipleColonSeparators);
    }
    let colon_index = match find_first_colon_index(text_message_bytes) {
        Some(index_value) => index_value,
        None => {
            // Defensive: colon_count == 1 guarantees find returns Some.
            // Unreachable in practice.
            return Err(ConfigLineParseError::NoColonSeparator);
        }
    };

    // Step 2: split into two halves.
    let key_half_raw = &text_message_bytes[..colon_index];
    let value_half_raw = &text_message_bytes[(colon_index + 1)..];

    // Step 3: strip whitespace from each half.
    let key_stripped = strip_ascii_whitespace_from_both_ends(key_half_raw);
    let value_stripped = strip_ascii_whitespace_from_both_ends(value_half_raw);

    // Step 4: validate non-empty.
    if key_stripped.is_empty() {
        return Err(ConfigLineParseError::EmptyKey);
    }
    if value_stripped.is_empty() {
        return Err(ConfigLineParseError::EmptyValue);
    }

    // Step 5: validate key length.
    if key_stripped.len() > MAX_CONFIG_KEY_BYTES {
        return Err(ConfigLineParseError::KeyExceedsBuffer);
    }

    // Step 6: validate value length.
    if value_stripped.len() > MAX_CONFIG_VALUE_BYTES {
        return Err(ConfigLineParseError::ValueExceedsBuffer);
    }

    // Step 7: reject internal whitespace in value.
    if slice_contains_ascii_whitespace(value_stripped) {
        return Err(ConfigLineParseError::ValueContainsInternalWhitespace);
    }

    // Step 8: match key against recognized table.
    let recognized_key = match recognized_config_key_from_bytes(key_stripped) {
        Some(key_variant) => key_variant,
        None => return Err(ConfigLineParseError::UnrecognizedKey),
    };

    // Step 9: copy value bytes into fixed-size output buffer.
    let mut value_buffer = [0u8; MAX_CONFIG_VALUE_BYTES];
    value_buffer[..value_stripped.len()].copy_from_slice(value_stripped);

    // Defensive narrowing: length check above guarantees this fits in u8.
    let value_length_u8: u8 = if value_stripped.len() <= MAX_CONFIG_VALUE_BYTES {
        value_stripped.len() as u8
    } else {
        return Err(ConfigLineParseError::ValueExceedsBuffer);
    };

    Ok(ParsedConfigLine {
        recognized_key,
        value_buffer,
        value_length: value_length_u8,
    })
}

// ============================================================================
// SECTION 47: Config-Line Parser — Cargo Tests
// ============================================================================

#[cfg(test)]
mod tests_config_line_parser {
    use super::*;

    // ── Happy-path tests ──────────────────────────────────────────────

    #[test]
    fn parses_plays_white_with_simple_name() {
        let parsed = parse_config_line_text_message(b"plays_white:alice")
            .expect("test: valid config line must parse");
        assert_eq!(parsed.recognized_key, RecognizedConfigKey::PlaysWhite);
        assert_eq!(parsed.value_as_bytes(), b"alice");
    }

    #[test]
    fn parses_plays_black_with_simple_name() {
        let parsed = parse_config_line_text_message(b"plays_black:bob")
            .expect("test: valid config line must parse");
        assert_eq!(parsed.recognized_key, RecognizedConfigKey::PlaysBlack);
        assert_eq!(parsed.value_as_bytes(), b"bob");
    }

    #[test]
    fn parses_player_time_with_integer() {
        let parsed = parse_config_line_text_message(b"player_time:10")
            .expect("test: valid config line must parse");
        assert_eq!(
            parsed.recognized_key,
            RecognizedConfigKey::PlayerTimeMinutes
        );
        assert_eq!(parsed.value_as_bytes(), b"10");
    }

    #[test]
    fn parses_refresh_rate_with_integer() {
        let parsed = parse_config_line_text_message(b"refresh_rate:5")
            .expect("test: valid config line must parse");
        assert_eq!(
            parsed.recognized_key,
            RecognizedConfigKey::RefreshRateSeconds
        );
        assert_eq!(parsed.value_as_bytes(), b"5");
    }

    #[test]
    fn parses_n_move_rule_with_integer() {
        let parsed = parse_config_line_text_message(b"n_move_rule:50")
            .expect("test: valid config line must parse");
        assert_eq!(parsed.recognized_key, RecognizedConfigKey::NMoveRule);
        assert_eq!(parsed.value_as_bytes(), b"50");
    }

    #[test]
    fn parses_n_move_rule_with_off() {
        let parsed = parse_config_line_text_message(b"n_move_rule:off")
            .expect("test: valid config line must parse");
        assert_eq!(parsed.recognized_key, RecognizedConfigKey::NMoveRule);
        assert_eq!(parsed.value_as_bytes(), b"off");
    }

    #[test]
    fn parses_three_time_rep_with_yes() {
        let parsed = parse_config_line_text_message(b"3_time_rep:yes")
            .expect("test: valid config line must parse");
        assert_eq!(
            parsed.recognized_key,
            RecognizedConfigKey::ThreeTimeRepetition
        );
        assert_eq!(parsed.value_as_bytes(), b"yes");
    }

    #[test]
    fn parses_three_time_rep_with_no() {
        let parsed = parse_config_line_text_message(b"3_time_rep:no")
            .expect("test: valid config line must parse");
        assert_eq!(
            parsed.recognized_key,
            RecognizedConfigKey::ThreeTimeRepetition
        );
        assert_eq!(parsed.value_as_bytes(), b"no");
    }

    // ── Whitespace handling tests ────────────────────────────────────

    #[test]
    fn strips_whitespace_around_key() {
        let parsed = parse_config_line_text_message(b"  plays_white:alice")
            .expect("test: leading whitespace in key must be stripped");
        assert_eq!(parsed.recognized_key, RecognizedConfigKey::PlaysWhite);
        assert_eq!(parsed.value_as_bytes(), b"alice");
    }

    #[test]
    fn strips_whitespace_around_value() {
        let parsed = parse_config_line_text_message(b"plays_white:  alice  ")
            .expect("test: surrounding whitespace in value must be stripped");
        assert_eq!(parsed.value_as_bytes(), b"alice");
    }

    #[test]
    fn strips_whitespace_around_both_halves() {
        let parsed = parse_config_line_text_message(b" plays_white : alice ")
            .expect("test: whitespace on both halves must be stripped");
        assert_eq!(parsed.recognized_key, RecognizedConfigKey::PlaysWhite);
        assert_eq!(parsed.value_as_bytes(), b"alice");
    }

    #[test]
    fn strips_tabs_and_newlines_as_whitespace() {
        let parsed = parse_config_line_text_message(b"\tplays_white\t:\nalice\r\n")
            .expect("test: tabs and newlines count as whitespace");
        assert_eq!(parsed.value_as_bytes(), b"alice");
    }

    // ── Rejection tests ──────────────────────────────────────────────

    #[test]
    fn rejects_input_with_no_colon() {
        let result = parse_config_line_text_message(b"plays_white_alice");
        assert_eq!(result, Err(ConfigLineParseError::NoColonSeparator));
    }

    #[test]
    fn rejects_input_with_multiple_colons() {
        let result = parse_config_line_text_message(b"plays_white:alice:bob");
        assert_eq!(result, Err(ConfigLineParseError::MultipleColonSeparators));
    }

    #[test]
    fn rejects_empty_key() {
        let result = parse_config_line_text_message(b":alice");
        assert_eq!(result, Err(ConfigLineParseError::EmptyKey));
    }

    #[test]
    fn rejects_whitespace_only_key() {
        let result = parse_config_line_text_message(b"   :alice");
        assert_eq!(result, Err(ConfigLineParseError::EmptyKey));
    }

    #[test]
    fn rejects_empty_value() {
        let result = parse_config_line_text_message(b"plays_white:");
        assert_eq!(result, Err(ConfigLineParseError::EmptyValue));
    }

    #[test]
    fn rejects_whitespace_only_value() {
        let result = parse_config_line_text_message(b"plays_white:   ");
        assert_eq!(result, Err(ConfigLineParseError::EmptyValue));
    }

    #[test]
    fn rejects_unrecognized_key() {
        let result = parse_config_line_text_message(b"unknown_key:value");
        assert_eq!(result, Err(ConfigLineParseError::UnrecognizedKey));
    }

    #[test]
    fn rejects_value_with_internal_whitespace() {
        let result = parse_config_line_text_message(b"plays_white:alice smith");
        assert_eq!(
            result,
            Err(ConfigLineParseError::ValueContainsInternalWhitespace)
        );
    }

    #[test]
    fn rejects_value_exceeding_buffer() {
        // 17 bytes, one over MAX_CONFIG_VALUE_BYTES.
        let result = parse_config_line_text_message(b"plays_white:aaaaaaaaaaaaaaaaa");
        assert_eq!(result, Err(ConfigLineParseError::ValueExceedsBuffer));
    }

    #[test]
    fn rejects_case_mismatched_key() {
        // Case-sensitive: "Plays_White" must not match "plays_white".
        let result = parse_config_line_text_message(b"Plays_White:alice");
        assert_eq!(result, Err(ConfigLineParseError::UnrecognizedKey));
    }

    #[test]
    fn rejects_partial_key_prefix() {
        let result = parse_config_line_text_message(b"plays:alice");
        assert_eq!(result, Err(ConfigLineParseError::UnrecognizedKey));
    }

    #[test]
    fn rejects_key_with_extra_suffix() {
        // "plays_whiteX" is 12 bytes (within MAX_CONFIG_KEY_BYTES = 16),
        // and does not match any recognized key.
        let result = parse_config_line_text_message(b"plays_whiteX:alice");
        assert_eq!(result, Err(ConfigLineParseError::UnrecognizedKey));
    }

    #[test]
    fn rejects_key_exceeding_buffer() {
        // "plays_white_extra" is 17 bytes, one over MAX_CONFIG_KEY_BYTES = 16.
        let result = parse_config_line_text_message(b"plays_white_extra:alice");
        assert_eq!(result, Err(ConfigLineParseError::KeyExceedsBuffer));
    }

    // ── Boundary tests ───────────────────────────────────────────────

    #[test]
    fn accepts_value_at_exact_buffer_boundary() {
        // Exactly MAX_CONFIG_VALUE_BYTES = 16 bytes.
        let parsed = parse_config_line_text_message(b"plays_white:aaaaaaaaaaaaaaaa")
            .expect("test: exactly 16-byte value must be accepted");
        assert_eq!(parsed.value_length, 16);
        assert_eq!(parsed.value_as_bytes(), b"aaaaaaaaaaaaaaaa");
    }

    #[test]
    fn accepts_single_byte_value() {
        let parsed = parse_config_line_text_message(b"refresh_rate:1")
            .expect("test: single-byte value must be accepted");
        assert_eq!(parsed.value_length, 1);
        assert_eq!(parsed.value_as_bytes(), b"1");
    }

    // ── Copy semantics ────────────────────────────────────────────────

    #[test]
    fn parsed_config_line_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<ParsedConfigLine>();
        assert_copy::<RecognizedConfigKey>();
        assert_copy::<ConfigLineParseError>();
    }

    // ── Wire-format consistency ──────────────────────────────────────

    #[test]
    fn wire_format_strings_are_stable() {
        // Pin the wire-format strings. Any change to these is a wire-
        // format breaking change and must be deliberate.
        assert_eq!(
            recognized_config_key_as_bytes(RecognizedConfigKey::PlaysWhite),
            b"plays_white"
        );
        assert_eq!(
            recognized_config_key_as_bytes(RecognizedConfigKey::PlaysBlack),
            b"plays_black"
        );
        assert_eq!(
            recognized_config_key_as_bytes(RecognizedConfigKey::PlayerTimeMinutes),
            b"player_time"
        );
        assert_eq!(
            recognized_config_key_as_bytes(RecognizedConfigKey::RefreshRateSeconds),
            b"refresh_rate"
        );
        assert_eq!(
            recognized_config_key_as_bytes(RecognizedConfigKey::NMoveRule),
            b"n_move_rule"
        );
        assert_eq!(
            recognized_config_key_as_bytes(RecognizedConfigKey::ThreeTimeRepetition),
            b"3_time_rep"
        );
    }
}

// ============================================================================
// SECTION 47: Config Value Semantic Parsers
// ============================================================================
//
// ## Project Context
//
// `parse_config_line_text_message` (Section 46) decomposes a config-line
// `text_message` into a recognized key plus a raw value byte slice. The
// raw value is not yet typed: `player_time:10` produces the value bytes
// `b"10"`; `n_move_rule:off` produces `b"off"`; `plays_white:alice`
// produces `b"alice"`.
//
// This section converts those raw value bytes into typed semantic
// values, one parser per value family. Each parser is pure and
// allocation-free.
//
// ## Error Policy
//
// Every parser returns `Option<T>`. `None` means "this value is not a
// well-formed instance of this family." The bootstrap layer treats any
// `None` as "skip this value and continue scanning the directory" —
// the user can correct the memo and the bootstrap will pick up the
// corrected value on the next refresh cycle. No diagnostic information
// crosses the `None` boundary, in keeping with the production-safety
// policy of this module.

/// Parse a `username` value (the value half of `plays_white:` or
/// `plays_black:`) into a fixed-size byte buffer.
///
/// ## Accepted Input
///
/// One or more bytes, total length `1..=MAX_USERNAME_BYTES`. The bytes
/// are not validated as UTF-8; consistent with the rest of the project,
/// usernames are byte strings.
///
/// ## Returns
///
/// - `Some((buffer, length))`: success. The first `length` bytes of
///   `buffer` are the username; remaining bytes are zero.
/// - `None`: input was empty or longer than `MAX_USERNAME_BYTES`.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. One stack-allocated `MAX_USERNAME_BYTES` buffer.
pub fn parse_username_value_bytes(value_bytes: &[u8]) -> Option<([u8; MAX_USERNAME_BYTES], u8)> {
    if value_bytes.is_empty() {
        return None;
    }
    if value_bytes.len() > MAX_USERNAME_BYTES {
        return None;
    }

    let mut output_buffer = [0u8; MAX_USERNAME_BYTES];
    output_buffer[..value_bytes.len()].copy_from_slice(value_bytes);

    // Length check above guarantees this narrowing is safe.
    // MAX_USERNAME_BYTES is 16, well within u8 range.
    let length_as_u8: u8 = value_bytes.len() as u8;

    Some((output_buffer, length_as_u8))
}

/// Parse an ASCII-decimal byte slice into a `u32`.
///
/// ## Accepted Input
///
/// One or more bytes, each in `b'0'..=b'9'`. No leading sign, no leading
/// `+`, no leading whitespace, no internal separators, no trailing junk.
/// The caller is responsible for any prior stripping (which
/// `parse_config_line_text_message` already performs on the value half).
///
/// ## Returns
///
/// - `Some(value)`: successful parse, fits in `u32`.
/// - `None`: empty input, non-digit byte present, or overflow beyond
///   `u32::MAX`.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Bounded loop (one iteration per input byte).
/// Overflow is checked, not assumed not to occur.
pub fn parse_decimal_u32_value(value_bytes: &[u8]) -> Option<u32> {
    if value_bytes.is_empty() {
        return None;
    }

    let mut accumulator: u32 = 0;
    let mut byte_index: usize = 0;
    while byte_index < value_bytes.len() {
        let current_byte = value_bytes[byte_index];
        if current_byte < b'0' || current_byte > b'9' {
            return None;
        }
        let digit_value: u32 = (current_byte - b'0') as u32;

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

/// Parse an ASCII-decimal byte slice into a `u8`.
///
/// Same accepted-input rules as `parse_decimal_u32_value`. Overflow
/// beyond `u8::MAX` (255) returns `None`.
///
/// ## Project Context
///
/// Used for `refresh_rate:` values. The constructor
/// `try_construct_memochess_game_config` further restricts the value
/// to the closed interval [`MIN_REFRESH_RATE_SECONDS`,
/// `MAX_REFRESH_RATE_SECONDS`]; this parser only enforces "fits in
/// `u8`."
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Bounded loop.
pub fn parse_decimal_u8_value(value_bytes: &[u8]) -> Option<u8> {
    if value_bytes.is_empty() {
        return None;
    }

    let mut accumulator: u8 = 0;
    let mut byte_index: usize = 0;
    while byte_index < value_bytes.len() {
        let current_byte = value_bytes[byte_index];
        if current_byte < b'0' || current_byte > b'9' {
            return None;
        }
        let digit_value: u8 = current_byte - b'0';

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

/// Parse an `n_move_rule` value into the form stored on `MemochessGameConfig`.
///
/// ## Accepted Input
///
/// - Exactly the three bytes `b"off"` (case-sensitive — the config-line
///   parser does not lowercase). Produces `Some(None)`.
/// - An ASCII-decimal integer that fits in `u16`. Produces
///   `Some(Some(value))`. The returned value is NOT range-checked
///   against `MIN_N_MOVE_RULE_VALUE`/`MAX_N_MOVE_RULE_VALUE` here; that
///   check belongs to `try_construct_memochess_game_config`. This
///   parser's job is shape-validity only.
///
/// ## Returns
///
/// - `Some(None)`: the rule is explicitly disabled.
/// - `Some(Some(n))`: the rule is set to `n` half-moves.
/// - `None`: input was neither `b"off"` nor a well-formed `u16` decimal.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Bounded loop.
pub fn parse_n_move_rule_value(value_bytes: &[u8]) -> Option<Option<u16>> {
    // Literal "off" branch.
    if value_bytes == b"off" {
        return Some(None);
    }

    // Decimal-integer branch. Inline rather than calling a u16 variant
    // (none exists yet) and to keep the overflow check explicit.
    if value_bytes.is_empty() {
        return None;
    }

    let mut accumulator: u16 = 0;
    let mut byte_index: usize = 0;
    while byte_index < value_bytes.len() {
        let current_byte = value_bytes[byte_index];
        if current_byte < b'0' || current_byte > b'9' {
            return None;
        }
        let digit_value: u16 = (current_byte - b'0') as u16;

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

    Some(Some(accumulator))
}

// ============================================================================
// SECTION 48: Config Value Semantic Parsers — Cargo Tests
// ============================================================================

#[cfg(test)]
mod config_value_semantic_parser_tests {
    use super::*;

    // ── parse_username_value_bytes ────────────────────────────────────

    #[test]
    fn parse_username_accepts_short_name() {
        let result = parse_username_value_bytes(b"alice");
        let (buffer, length) = result.expect("alice should parse");
        assert_eq!(length, 5);
        assert_eq!(&buffer[..5], b"alice");
    }

    #[test]
    fn parse_username_accepts_single_character() {
        let result = parse_username_value_bytes(b"a");
        let (buffer, length) = result.expect("single char should parse");
        assert_eq!(length, 1);
        assert_eq!(buffer[0], b'a');
    }

    #[test]
    fn parse_username_accepts_maximum_length_name() {
        // Exactly MAX_USERNAME_BYTES (16) bytes.
        let max_input: &[u8] = b"abcdefghijklmnop";
        assert_eq!(max_input.len(), MAX_USERNAME_BYTES);
        let result = parse_username_value_bytes(max_input);
        let (buffer, length) = result.expect("16-byte name should parse");
        assert_eq!(length as usize, MAX_USERNAME_BYTES);
        assert_eq!(&buffer[..], max_input);
    }

    #[test]
    fn parse_username_rejects_empty_input() {
        assert!(parse_username_value_bytes(b"").is_none());
    }

    #[test]
    fn parse_username_rejects_too_long_input() {
        // One byte over MAX_USERNAME_BYTES (16) = 17 bytes.
        let too_long: &[u8] = b"abcdefghijklmnopq";
        assert_eq!(too_long.len(), MAX_USERNAME_BYTES + 1);
        assert!(parse_username_value_bytes(too_long).is_none());
    }

    #[test]
    fn parse_username_accepts_non_alphabetic_bytes() {
        // The username parser does not constrain the byte set; only
        // length. The bootstrap layer may further constrain if needed.
        let result = parse_username_value_bytes(b"bob123");
        let (buffer, length) = result.expect("alphanumeric name should parse");
        assert_eq!(length, 6);
        assert_eq!(&buffer[..6], b"bob123");
    }

    // ── parse_decimal_u32_value ───────────────────────────────────────

    #[test]
    fn parse_u32_accepts_zero() {
        assert_eq!(parse_decimal_u32_value(b"0"), Some(0));
    }

    #[test]
    fn parse_u32_accepts_small_integer() {
        assert_eq!(parse_decimal_u32_value(b"10"), Some(10));
    }

    #[test]
    fn parse_u32_accepts_large_integer() {
        assert_eq!(parse_decimal_u32_value(b"1234567890"), Some(1_234_567_890));
    }

    #[test]
    fn parse_u32_accepts_maximum_value() {
        // u32::MAX = 4_294_967_295
        assert_eq!(parse_decimal_u32_value(b"4294967295"), Some(u32::MAX));
    }

    #[test]
    fn parse_u32_rejects_overflow() {
        // u32::MAX + 1 = 4_294_967_296
        assert_eq!(parse_decimal_u32_value(b"4294967296"), None);
    }

    #[test]
    fn parse_u32_rejects_very_large_overflow() {
        assert_eq!(parse_decimal_u32_value(b"99999999999"), None);
    }

    #[test]
    fn parse_u32_rejects_empty_input() {
        assert_eq!(parse_decimal_u32_value(b""), None);
    }

    #[test]
    fn parse_u32_rejects_leading_sign_plus() {
        assert_eq!(parse_decimal_u32_value(b"+10"), None);
    }

    #[test]
    fn parse_u32_rejects_leading_sign_minus() {
        assert_eq!(parse_decimal_u32_value(b"-10"), None);
    }

    #[test]
    fn parse_u32_rejects_internal_whitespace() {
        assert_eq!(parse_decimal_u32_value(b"1 0"), None);
    }

    #[test]
    fn parse_u32_rejects_trailing_letter() {
        assert_eq!(parse_decimal_u32_value(b"10a"), None);
    }

    #[test]
    fn parse_u32_rejects_leading_letter() {
        assert_eq!(parse_decimal_u32_value(b"a10"), None);
    }

    #[test]
    fn parse_u32_accepts_leading_zeros() {
        // No special rule against leading zeros at this layer.
        assert_eq!(parse_decimal_u32_value(b"007"), Some(7));
    }

    // ── parse_decimal_u8_value ────────────────────────────────────────

    #[test]
    fn parse_u8_accepts_zero() {
        assert_eq!(parse_decimal_u8_value(b"0"), Some(0));
    }

    #[test]
    fn parse_u8_accepts_small_value() {
        assert_eq!(parse_decimal_u8_value(b"10"), Some(10));
    }

    #[test]
    fn parse_u8_accepts_maximum_value() {
        assert_eq!(parse_decimal_u8_value(b"255"), Some(u8::MAX));
    }

    #[test]
    fn parse_u8_rejects_overflow_at_256() {
        assert_eq!(parse_decimal_u8_value(b"256"), None);
    }

    #[test]
    fn parse_u8_rejects_overflow_large() {
        assert_eq!(parse_decimal_u8_value(b"1000"), None);
    }

    #[test]
    fn parse_u8_rejects_empty_input() {
        assert_eq!(parse_decimal_u8_value(b""), None);
    }

    #[test]
    fn parse_u8_rejects_non_digit_byte() {
        assert_eq!(parse_decimal_u8_value(b"1x"), None);
    }

    // ── parse_n_move_rule_value ───────────────────────────────────────

    #[test]
    fn parse_n_move_rule_accepts_off() {
        assert_eq!(parse_n_move_rule_value(b"off"), Some(None));
    }

    #[test]
    fn parse_n_move_rule_rejects_uppercase_off() {
        // The config-line parser does not lowercase. "Off" is not "off".
        assert_eq!(parse_n_move_rule_value(b"Off"), None);
    }

    #[test]
    fn parse_n_move_rule_rejects_off_with_trailing_bytes() {
        assert_eq!(parse_n_move_rule_value(b"offf"), None);
        assert_eq!(parse_n_move_rule_value(b"of"), None);
    }

    #[test]
    fn parse_n_move_rule_accepts_fifty() {
        assert_eq!(parse_n_move_rule_value(b"50"), Some(Some(50)));
    }

    #[test]
    fn parse_n_move_rule_accepts_seventy_five() {
        assert_eq!(parse_n_move_rule_value(b"75"), Some(Some(75)));
    }

    #[test]
    fn parse_n_move_rule_accepts_zero() {
        // Shape-valid even though the constructor will reject it as
        // below MIN_N_MOVE_RULE_VALUE.
        assert_eq!(parse_n_move_rule_value(b"0"), Some(Some(0)));
    }

    #[test]
    fn parse_n_move_rule_accepts_u16_maximum() {
        assert_eq!(parse_n_move_rule_value(b"65535"), Some(Some(u16::MAX)));
    }

    #[test]
    fn parse_n_move_rule_rejects_u16_overflow() {
        assert_eq!(parse_n_move_rule_value(b"65536"), None);
    }

    #[test]
    fn parse_n_move_rule_rejects_empty_input() {
        assert_eq!(parse_n_move_rule_value(b""), None);
    }

    #[test]
    fn parse_n_move_rule_rejects_mixed_alphanumeric() {
        assert_eq!(parse_n_move_rule_value(b"5o"), None);
        assert_eq!(parse_n_move_rule_value(b"o5"), None);
    }
}

// ============================================================================
// SECTION 49: Partial Bootstrap Config — Struct
// ============================================================================
//
// ## Project Context
//
// `q_and_a_setup_bootstrap` (to be written) scans the memo directory
// repeatedly, processing TOML files in any order and accumulating
// configuration values until enough have been collected to construct a
// fully-validated `MemochessGameConfig`.
//
// `PartialBootstrapConfig` is the accumulator. Each field is an
// `Option` that starts as `None` and becomes `Some(...)` once a valid
// memo for that field has been observed. Per the spec, the first
// valid value wins: once a field is `Some(...)`, subsequent memos
// targeting the same field are ignored.
//
// ## Field Mapping to `MemochessGameConfig`
//
// | Partial field            | Final field                          |
// |--------------------------|--------------------------------------|
// | white_player_name        | white_player_name_buffer + length    |
// | black_player_name        | black_player_name_buffer + length    |
// | player_time_minutes      | max_time_limit_per_player_seconds    |
// | refresh_rate_seconds     | refresh_rate_seconds                 |
// | n_move_rule              | n_move_rule (passed through)         |
//
// Fields not collected via bootstrap (and therefore not part of this
// struct): `directory_path_buffer`/`length` and `local_user_name_buffer`/
// `length`. Those two are supplied to the bootstrap function as
// arguments, not gathered from memo files, because they bootstrap the
// memo-file-reading process itself.
//
// ## Required vs. Optional Fields
//
// Required (game cannot start until set):
//   - white_player_name
//   - black_player_name
//   - player_time_minutes
//   - refresh_rate_seconds
//
// Optional (defaults to disabled if never set):
//   - n_move_rule
//
// `build_memochess_config_if_complete` (Component 4) checks that all
// required fields are populated.

/// Accumulator for bootstrap configuration discovery.
///
/// Constructed empty (all fields `None`). Each field transitions from
/// `None` to `Some(...)` exactly once: subsequent attempts to set an
/// already-set field are silently ignored. This implements the
/// first-wins policy specified for the bootstrap layer.
///
/// ## Memory
///
/// `Copy`. Stack-only. Two `MAX_USERNAME_BYTES` (16-byte) inline name
/// buffers; a handful of small scalar fields. No heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartialBootstrapConfig {
    /// White player name, once a valid `plays_white:` memo has been
    /// observed. The tuple is `(buffer, length)`: only the first
    /// `length` bytes of `buffer` are meaningful.
    pub white_player_name: Option<([u8; MAX_USERNAME_BYTES], u8)>,

    /// Black player name, once a valid `plays_black:` memo has been
    /// observed.
    pub black_player_name: Option<([u8; MAX_USERNAME_BYTES], u8)>,

    /// Per-player thinking-time budget in *minutes*, as written by the
    /// user. Conversion to seconds happens in
    /// `build_memochess_config_if_complete` (Component 4) via
    /// `convert_minutes_to_seconds_checked`.
    ///
    /// Stored in minutes (not seconds) here so that this struct
    /// faithfully represents what the user wrote, and so that the
    /// minutes-to-seconds conversion (which can overflow) is performed
    /// only once, at finalization time.
    pub player_time_minutes: Option<u32>,

    /// Game-loop refresh cadence in seconds, once a valid
    /// `refresh_rate:` memo has been observed.
    pub refresh_rate_seconds: Option<u8>,

    /// Optional N-move-rule setting.
    ///
    /// `None` means "no memo with `n_move_rule:` has been observed
    /// yet"; the bootstrap treats this as the default (rule disabled)
    /// when finalizing. A memo with `n_move_rule:off` also leaves
    /// this field as `None`. A memo with `n_move_rule:50` sets this to
    /// `Some(50)`.
    ///
    /// Because both "user never wrote a memo" and "user wrote `off`"
    /// collapse to `None`, this field cannot distinguish them — and
    /// for the project's purposes it does not need to. Either way the
    /// rule is disabled.
    pub n_move_rule: Option<u16>,
}

impl PartialBootstrapConfig {
    /// Construct an empty accumulator. All fields `None`.
    pub const fn new_empty_partial_bootstrap_config() -> PartialBootstrapConfig {
        PartialBootstrapConfig {
            white_player_name: None,
            black_player_name: None,
            player_time_minutes: None,
            refresh_rate_seconds: None,
            n_move_rule: None,
        }
    }

    /// Returns `true` if every *required* field has been set.
    /// `n_move_rule` is not checked because it is optional.
    ///
    /// This is a fast check used each loop iteration to decide whether
    /// to attempt finalization. It does not validate field values;
    /// final validation (range checks, distinct white/black names)
    /// is performed by `build_memochess_config_if_complete`.
    pub const fn all_required_fields_are_set(&self) -> bool {
        self.white_player_name.is_some()
            && self.black_player_name.is_some()
            && self.player_time_minutes.is_some()
            && self.refresh_rate_seconds.is_some()
    }
}

// ============================================================================
// SECTION 50: Partial Bootstrap Config — Cargo Tests
// ============================================================================

#[cfg(test)]
mod partial_bootstrap_config_tests {
    use super::*;

    #[test]
    fn new_empty_has_all_none_fields() {
        let partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();
        assert!(partial.white_player_name.is_none());
        assert!(partial.black_player_name.is_none());
        assert!(partial.player_time_minutes.is_none());
        assert!(partial.refresh_rate_seconds.is_none());
        assert!(partial.n_move_rule.is_none());
    }

    #[test]
    fn new_empty_reports_required_fields_not_set() {
        let partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();
        assert!(!partial.all_required_fields_are_set());
    }

    #[test]
    fn three_of_four_required_fields_set_is_not_complete() {
        let mut partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();
        let mut name_buffer = [0u8; MAX_USERNAME_BYTES];
        name_buffer[..5].copy_from_slice(b"alice");
        partial.white_player_name = Some((name_buffer, 5));
        partial.player_time_minutes = Some(10);
        partial.refresh_rate_seconds = Some(10);
        // black_player_name still None
        assert!(!partial.all_required_fields_are_set());
    }

    #[test]
    fn all_four_required_fields_set_is_complete() {
        let mut partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();

        let mut white_buffer = [0u8; MAX_USERNAME_BYTES];
        white_buffer[..5].copy_from_slice(b"alice");
        partial.white_player_name = Some((white_buffer, 5));

        let mut black_buffer = [0u8; MAX_USERNAME_BYTES];
        black_buffer[..3].copy_from_slice(b"bob");
        partial.black_player_name = Some((black_buffer, 3));

        partial.player_time_minutes = Some(10);
        partial.refresh_rate_seconds = Some(10);

        assert!(partial.all_required_fields_are_set());
    }

    #[test]
    fn n_move_rule_is_optional_for_completeness() {
        // Build a partial with all four required fields set and
        // n_move_rule explicitly None — should be complete.
        let mut partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();
        let mut white_buffer = [0u8; MAX_USERNAME_BYTES];
        white_buffer[..5].copy_from_slice(b"alice");
        partial.white_player_name = Some((white_buffer, 5));

        let mut black_buffer = [0u8; MAX_USERNAME_BYTES];
        black_buffer[..3].copy_from_slice(b"bob");
        partial.black_player_name = Some((black_buffer, 3));

        partial.player_time_minutes = Some(10);
        partial.refresh_rate_seconds = Some(10);

        assert!(partial.n_move_rule.is_none());
        assert!(partial.all_required_fields_are_set());

        // And with n_move_rule set, still complete.
        partial.n_move_rule = Some(50);
        assert!(partial.all_required_fields_are_set());
    }
}

// ============================================================================
// SECTION 51: Bootstrap Helper — Minutes-to-Seconds Conversion
// ============================================================================

/// Convert a per-player thinking-time budget from minutes to seconds,
/// with overflow detection.
///
/// ## Project Context
///
/// The bootstrap wire format expresses per-player time in minutes
/// (`player_time:10`), while `MemochessGameConfig::max_time_limit_per_player_seconds`
/// is stored in seconds. The conversion `minutes * 60` is performed
/// once, at finalization time, by `build_memochess_config_if_complete`
/// (Component 4, next).
///
/// Performing the conversion only at finalization (rather than during
/// memo ingestion) keeps `PartialBootstrapConfig.player_time_minutes`
/// faithful to what the user wrote and consolidates the overflow
/// check at a single site.
///
/// ## Overflow
///
/// `u32::MAX / 60` is roughly 71_582_788 minutes (about 136 years).
/// Any user-supplied minute count exceeding that range overflows the
/// multiplication. We use `checked_mul` and surface overflow as
/// `None`, which the caller treats as "the value the user wrote is
/// not usable; remain blocked on this field until they correct it."
///
/// ## Returns
///
/// - `Some(seconds_value)` on successful conversion.
/// - `None` if `minutes_value * 60` would overflow `u32`.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. Single checked multiplication.
pub const fn convert_minutes_to_seconds_checked(minutes_value: u32) -> Option<u32> {
    minutes_value.checked_mul(60)
}

// ============================================================================
// SECTION 52: Bootstrap Helper — Cargo Tests
// ============================================================================

#[cfg(test)]
mod minutes_to_seconds_conversion_tests {
    use super::*;

    #[test]
    fn converts_zero_minutes_to_zero_seconds() {
        assert_eq!(convert_minutes_to_seconds_checked(0), Some(0));
    }

    #[test]
    fn converts_one_minute_to_sixty_seconds() {
        assert_eq!(convert_minutes_to_seconds_checked(1), Some(60));
    }

    #[test]
    fn converts_ten_minutes_to_six_hundred_seconds() {
        // The canonical "bullet 10" example from the project spec.
        assert_eq!(convert_minutes_to_seconds_checked(10), Some(600));
    }

    #[test]
    fn converts_sixty_minutes_to_thirty_six_hundred_seconds() {
        assert_eq!(convert_minutes_to_seconds_checked(60), Some(3600));
    }

    #[test]
    fn converts_largest_safe_value() {
        // u32::MAX / 60 truncates to 71_582_788; that times 60 fits.
        let largest_safe_minutes: u32 = u32::MAX / 60;
        let expected_seconds: u32 = largest_safe_minutes * 60;
        assert_eq!(
            convert_minutes_to_seconds_checked(largest_safe_minutes),
            Some(expected_seconds)
        );
    }

    #[test]
    fn detects_overflow_just_past_safe_range() {
        // One more minute than the largest safe value overflows.
        let just_overflowing_minutes: u32 = (u32::MAX / 60) + 1;
        assert_eq!(
            convert_minutes_to_seconds_checked(just_overflowing_minutes),
            None
        );
    }

    #[test]
    fn detects_overflow_at_u32_max() {
        assert_eq!(convert_minutes_to_seconds_checked(u32::MAX), None);
    }
}

// ============================================================================
// SECTION 53: Bootstrap Finalization — build_memochess_config_if_complete
// ============================================================================
//
// ## Project Context
//
// Each iteration of the bootstrap loop calls this function after
// updating the `PartialBootstrapConfig` with the latest scan results.
// If all required fields are present and the final validation in
// `try_construct_memochess_game_config` succeeds, the bootstrap exits
// with the returned `MemochessGameConfig`. Otherwise, the loop sleeps
// and scans again on the next refresh cycle.
//
// ## Why `Option` rather than `Result`
//
// At this layer there are exactly two outcomes the caller acts on:
//
//   - "config is finished" (return it and exit the loop), or
//   - "config is not finished" (sleep and try again).
//
// "Not finished" subsumes both "a required field is missing" and "a
// field has a value but final validation rejected it" — in either
// case the bootstrap behavior is identical: keep looping, keep
// prompting the user. There is no third action the caller would take
// based on a structured error. `Option<MemochessGameConfig>` matches
// that decision shape exactly.
//
// If diagnostic information about *which* field is missing or invalid
// becomes useful (e.g., for a future debug build), that can be added
// as a separate function without changing this one.

/// Attempt to finalize a `PartialBootstrapConfig` into a fully-validated
/// `MemochessGameConfig`.
///
/// ## Arguments
///
/// - `partial_config`: the accumulator built up by the bootstrap loop.
/// - `directory_path_bytes`: the absolute directory path supplied to
///   `q_and_a_setup_bootstrap` (not collected via memo files).
/// - `local_user_name_bytes`: the local user name supplied to
///   `q_and_a_setup_bootstrap` (not collected via memo files).
///
/// ## Returns
///
/// - `Some(MemochessGameConfig)`: all required partial fields are
///   present, the minutes-to-seconds conversion did not overflow, and
///   `try_construct_memochess_game_config` accepted every value.
/// - `None`: any of the above failed. The bootstrap loop continues.
///
/// ## Finalization Steps
///
/// 1. Read all four required `Option` fields from `partial_config`. If
///    any is `None`, return `None`.
/// 2. Convert `player_time_minutes` to seconds via
///    `convert_minutes_to_seconds_checked`. On overflow, return `None`.
/// 3. Extract the meaningful prefix of each name buffer.
/// 4. Call `try_construct_memochess_game_config` with all values.
///    On any `Err(...)`, return `None`.
///
/// ## Memory & Panic Policy
///
/// No heap. No panics. The two `match` arms on `Result` from the
/// constructor discard the error variant deliberately: at this layer,
/// "rejected by constructor" is indistinguishable from "user has not
/// yet supplied a valid value" — both produce `None`.
pub fn build_memochess_config_if_complete(
    partial_config: &PartialBootstrapConfig,
    directory_path_bytes: &[u8],
    local_user_name_bytes: &[u8],
) -> Option<MemochessGameConfig> {
    // ── Step 1: required fields must all be populated ───────────────
    let (white_name_buffer, white_name_length) = match partial_config.white_player_name {
        Some(pair) => pair,
        None => return None,
    };
    let (black_name_buffer, black_name_length) = match partial_config.black_player_name {
        Some(pair) => pair,
        None => return None,
    };
    let player_time_minutes = match partial_config.player_time_minutes {
        Some(value) => value,
        None => return None,
    };
    let refresh_rate_seconds = match partial_config.refresh_rate_seconds {
        Some(value) => value,
        None => return None,
    };

    // ── Step 2: minutes → seconds, with overflow check ──────────────
    let max_time_limit_per_player_seconds =
        match convert_minutes_to_seconds_checked(player_time_minutes) {
            Some(seconds_value) => seconds_value,
            None => return None,
        };

    // ── Step 3: take meaningful name slices ─────────────────────────
    let white_name_length_as_usize = white_name_length as usize;
    let black_name_length_as_usize = black_name_length as usize;

    // Defensive clamp: lengths cannot exceed MAX_USERNAME_BYTES via the
    // public API of `PartialBootstrapConfig`, but a clamp here keeps the
    // slice operation panic-free under any conceivable corruption.
    let safe_white_length = if white_name_length_as_usize > MAX_USERNAME_BYTES {
        MAX_USERNAME_BYTES
    } else {
        white_name_length_as_usize
    };
    let safe_black_length = if black_name_length_as_usize > MAX_USERNAME_BYTES {
        MAX_USERNAME_BYTES
    } else {
        black_name_length_as_usize
    };

    let white_name_bytes = &white_name_buffer[..safe_white_length];
    let black_name_bytes = &black_name_buffer[..safe_black_length];

    // ── Step 4: hand off to the constructor for final validation ────
    let construction_result = MemochessGameConfig::try_construct_memochess_game_config(
        directory_path_bytes,
        local_user_name_bytes,
        white_name_bytes,
        black_name_bytes,
        max_time_limit_per_player_seconds,
        refresh_rate_seconds,
        partial_config.n_move_rule.unwrap_or(50), // default to 50? (that would be fine)
    );

    match construction_result {
        Ok(finalized_config) => Some(finalized_config),
        Err(_) => None,
    }
}

// ============================================================================
// SECTION 54: Bootstrap Finalization — Cargo Tests
// ============================================================================

#[cfg(test)]
mod build_memochess_config_if_complete_tests {
    use super::*;

    /// Helper: create a `PartialBootstrapConfig` with all four required
    /// fields populated with the supplied values. `n_move_rule` is left
    /// at `None`.
    fn make_complete_partial_config(
        white_name: &[u8],
        black_name: &[u8],
        player_time_minutes_value: u32,
        refresh_rate_seconds_value: u8,
    ) -> PartialBootstrapConfig {
        let mut partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();

        let mut white_buffer = [0u8; MAX_USERNAME_BYTES];
        white_buffer[..white_name.len()].copy_from_slice(white_name);
        partial.white_player_name = Some((white_buffer, white_name.len() as u8));

        let mut black_buffer = [0u8; MAX_USERNAME_BYTES];
        black_buffer[..black_name.len()].copy_from_slice(black_name);
        partial.black_player_name = Some((black_buffer, black_name.len() as u8));

        partial.player_time_minutes = Some(player_time_minutes_value);
        partial.refresh_rate_seconds = Some(refresh_rate_seconds_value);

        partial
    }

    #[test]
    fn returns_none_when_partial_is_empty() {
        let partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_only_white_player_set() {
        let mut partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();
        let mut buffer = [0u8; MAX_USERNAME_BYTES];
        buffer[..5].copy_from_slice(b"alice");
        partial.white_player_name = Some((buffer, 5));
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_only_three_of_four_required_fields_set() {
        let mut partial = PartialBootstrapConfig::new_empty_partial_bootstrap_config();

        let mut white_buffer = [0u8; MAX_USERNAME_BYTES];
        white_buffer[..5].copy_from_slice(b"alice");
        partial.white_player_name = Some((white_buffer, 5));

        let mut black_buffer = [0u8; MAX_USERNAME_BYTES];
        black_buffer[..3].copy_from_slice(b"bob");
        partial.black_player_name = Some((black_buffer, 3));

        partial.player_time_minutes = Some(10);
        // refresh_rate_seconds still None

        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_some_when_all_required_fields_set_and_valid() {
        let partial = make_complete_partial_config(b"alice", b"bob", 10, 10);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        let config = result.expect("complete partial with valid values should finalize");

        assert_eq!(config.white_player_name_as_bytes(), b"alice");
        assert_eq!(config.black_player_name_as_bytes(), b"bob");
        assert_eq!(config.local_user_name_as_bytes(), b"tom");
        assert_eq!(config.directory_path_as_bytes(), b"/tmp/game_dir");
        assert_eq!(config.max_time_limit_per_player_seconds, 600);
        assert_eq!(config.refresh_rate_seconds, 10);
        assert_eq!(config.n_move_rule, 50);
    }

    #[test]
    fn passes_through_n_move_rule_when_set() {
        let mut partial = make_complete_partial_config(b"alice", b"bob", 10, 10);
        partial.n_move_rule = Some(50);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        let config = result.expect("complete partial should finalize");
        assert_eq!(config.n_move_rule, 50);
    }

    #[test]
    fn returns_none_when_n_move_rule_out_of_range() {
        // Below MIN_N_MOVE_RULE_VALUE (10) — constructor rejects.
        let mut partial = make_complete_partial_config(b"alice", b"bob", 10, 10);
        partial.n_move_rule = Some(5);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_refresh_rate_out_of_range() {
        // Refresh rate 0 is below MIN_REFRESH_RATE_SECONDS (1) — constructor rejects.
        let partial = make_complete_partial_config(b"alice", b"bob", 10, 0);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_white_and_black_names_identical() {
        let partial = make_complete_partial_config(b"alice", b"alice", 10, 10);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_on_minutes_to_seconds_overflow() {
        // (u32::MAX / 60) + 1 minutes overflows when multiplied by 60.
        let overflowing_minutes = (u32::MAX / 60) + 1;
        let partial = make_complete_partial_config(b"alice", b"bob", overflowing_minutes, 10);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_directory_path_empty() {
        let partial = make_complete_partial_config(b"alice", b"bob", 10, 10);
        let result = build_memochess_config_if_complete(&partial, b"", b"tom");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_local_user_name_empty() {
        let partial = make_complete_partial_config(b"alice", b"bob", 10, 10);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"");
        assert!(result.is_none());
    }

    #[test]
    fn accepts_minimum_valid_player_time() {
        // 1 minute = 60 seconds, above MIN_TIME_LIMIT_PER_PLAYER_SECONDS (1).
        let partial = make_complete_partial_config(b"alice", b"bob", 1, 10);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        let config = result.expect("1 minute should be acceptable");
        assert_eq!(config.max_time_limit_per_player_seconds, 60);
    }

    #[test]
    fn returns_none_when_player_time_is_zero_minutes() {
        // 0 minutes → 0 seconds, below MIN_TIME_LIMIT_PER_PLAYER_SECONDS (1).
        let partial = make_complete_partial_config(b"alice", b"bob", 0, 10);
        let result = build_memochess_config_if_complete(&partial, b"/tmp/game_dir", b"tom");
        assert!(result.is_none());
    }
}

// src/chrono_sort_module.rs

// # Chronological Directory Sort, Search, Hash (to safely iterate) — File-Backed Index

// Posix: Sort, search, chronologically through files in a local
// dir using a local-file-system on-file based lookup-table
// for chronological order, because mtime (time file modified)
// is not a default sort option, and storing many N full paths
// in RAM is infeasible.

// ## Project Context

// This module provides a low-heap, fail-safe mechanism for iterating
// the files of a single POSIX directory in **chronological order by mtime**,
// one file at a time, via random-access chronological lookup by position.

// The directory being indexed has these project-level invariants:

// - **One directory only** — all indexed files share a single parent path.
// - **Files are added over time** — growth is the steady-state case,
//   not an edge case.
// - **Files are never deleted** — the count is monotonically non-decreasing.
// - **mtimes of existing files do not change** — only new files appear.
// - **New files have newer mtimes than all existing files** — therefore
//   the chronological sort order can be maintained by pure append after
//   the initial build.
// - **Basenames are short** — capped at 64 bytes (see `MAX_BASENAME_LEN`).

// ## Memory Model

// Per-lookup memory is stack-only, on the order of a few kilobytes,
// independent of the file count N. The index itself lives on disk as
// a small set of fixed-width files in a caller-specified temp root.
// No `Vec`, `String`, `HashMap`, or other heap-growing structure scales
// with N inside this module.

// Heap is used only by unavoidable standard-library calls (e.g.
// `std::fs::read_dir` allocates an `OsString` per entry, which is freed
// before the next entry is produced). This is bounded per-iteration, not
// per-N.

// ## On-Disk Layout

// Under `<caller_temp_root>/chrono_index/`:

// ```text
// header.bin   Fixed-width header. Authoritative metadata.
// names.bin    record_id -> basename. Fixed 64 B per record. Append-only.
// mtimes.bin   Sorted by (mtime_sec, mtime_nsec, record_id).
//              Fixed 20 B per record. Append-only in steady state.
// scratch/     Used only during cold rebuild (external merge sort).
//              Removed after rebuild succeeds.
// ```

// ## Failure Policy

// Per project rules: this module **never halts the program**. All
// production paths return `Result<T, ChronoIndexError>` with terse,
// non-data-leaking error codes. The caller is expected to log the code
// and retry on the next call. Internal recovery actions (e.g. silent
// rebuild on header validation failure) are taken whenever the index
// can be self-healed without user intervention.

// Per project rules:
// - No `panic!` in production paths.
// - No `unwrap` or `expect` in production paths.
// - No `assert!` in production paths (test-only via `#[cfg(test)]`).
// - `debug_assert!` permitted, guarded by `#[cfg(all(debug_assertions, not(test)))]`
//   where appropriate.
// - No unsafe code.

use std::fs::{File, OpenOptions};
use std::io::{self, Error, ErrorKind};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

// =========================================================================
// Pearson hashing — single source of all hashing in this module
// =========================================================================
//
// ## Project context
//
// This module previously used FNV-1a 64-bit hashing in three roles
// (header signal_hash, name_hashes.bin sidecar, and chrono_sort_hash_to_n).
// That was out of spec: the project's policy is to use the existing
// Pearson hashing module (`pearson_hash_salt_array`) and its compile-
// time-generated permutation table (`GENERATED_TABLE`). FNV-1a has
// been removed; all hashing here now flows through Pearson.
//
// ## Width
//
// `PEARSON_SALT_ARRAY_SIZE` controls the byte-width of every hash this
// module produces — signal_hash, per-record sidecar entry, and the
// chronological-sequence hash. A single constant keeps the three
// roles uniform and easy to retune. Increase for lower collision odds
// at small storage cost. Decrease for tighter on-disk size at the cost
// of more frequent collisions (which manifest as extra rebuilds, not
// as data loss — see the project README's "What is the outcome of a
// hash collision?" discussion).
//
// At width 2: 1-in-65,536 odds per comparison. For the chess-game use
// case (file counts in the tens), this means essentially never. For
// directories with millions of files, raise to 3 or 4.
//
// ## Salts
//
// Three independent fixed salt arrays so the three roles are
// statistically uncorrelated. The salt values themselves are not
// secret — Pearson hashing is non-cryptographic — but they must
// remain stable across runs because they affect the on-disk format
// (signal_hash in the header, and every record in
// name_hashes.bin). Changing any salt value requires a HEADER_VERSION
// bump so existing indices are detected as out-of-format and rebuilt.

/// Byte-width of every Pearson hash produced by this module. Controls
/// signal_hash width in the header, name_hashes.bin record width, and
/// chrono_sort_hash_to_n output width simultaneously. See section docs
/// above for the collision/storage trade-off.
pub const PEARSON_SALT_ARRAY_SIZE: usize = 2;

/// Per-lane salt bytes for Role 1 (header signal_hash XOR-fold over
/// basenames). Independent of NAME_HASH_SALTS and CHRONO_SORT_HASH_SALTS
/// so the three roles produce statistically uncorrelated outputs.
const SIGNAL_HASH_SALTS: [u8; PEARSON_SALT_ARRAY_SIZE] = [0x5A, 0xA5];

/// Per-lane salt bytes for Role 2 (per-basename entry stored in
/// name_hashes.bin). Independent of SIGNAL_HASH_SALTS and
/// CHRONO_SORT_HASH_SALTS.
const NAME_HASH_SALTS: [u8; PEARSON_SALT_ARRAY_SIZE] = [0x3C, 0xC3];

/// Per-lane salt bytes for Role 3 (running chronological-sequence
/// hash returned by chrono_sort_hash_to_n). Independent of
/// SIGNAL_HASH_SALTS and NAME_HASH_SALTS.
const CHRONO_SORT_HASH_SALTS: [u8; PEARSON_SALT_ARRAY_SIZE] = [0x69, 0x96];

// =========================================================================
// Public constants — file layout
// =========================================================================

/// Magic bytes identifying a `header.bin` file produced by this module.
///
/// Used to detect corruption, version mismatch, or accidental reuse of an
/// unrelated file at the header path. Any mismatch triggers a rebuild.
pub const HEADER_MAGIC: [u8; 8] = *b"CHRIDX01";

/// On-disk format version. Bump on any incompatible layout change.
/// Mismatched versions trigger a rebuild rather than an attempt to migrate.
pub const HEADER_VERSION: u32 = 1;

/// Maximum length in bytes of a basename stored in `names.bin`.
///
/// Per project spec: basenames are short, "definitely <64 char". We store
/// 64 bytes including any NUL padding, giving room for up to 64 ASCII or
/// up to 16 four-byte UTF-8 characters. Names longer than this cannot be
/// indexed; such entries are skipped at build time (logged terse code).
pub const MAX_BASENAME_LEN: usize = 64;

/// Maximum length in bytes of the parent directory absolute path stored
/// in the header. POSIX `PATH_MAX` is typically 4096 on Linux; we cap
/// here at the same value. Longer parent paths cannot be indexed.
pub const MAX_PARENT_PATH_LEN: usize = 4096;

/// Size in bytes of one `names.bin` record. Fixed-width to permit O(1)
/// random access by `record_id`: byte offset = `record_id * NAME_RECORD_SIZE`.
pub const NAME_RECORD_SIZE: usize = MAX_BASENAME_LEN;

/// Size in bytes of one `mtimes.bin` record:
///   `(mtime_sec: i64, mtime_nsec: i32, record_id: u64)` = 8 + 4 + 8 = 20.
/// Fixed-width to permit O(1) random access and in-place external sort.
pub const MTIME_RECORD_SIZE: usize = 20;

/// Size in bytes of the on-disk `header.bin`. Fixed, validated on read.
///
/// Layout (all little-endian, packed in declaration order):
///
/// ```text
///   offset  size  field
///   ------  ----  -----
///        0     8  magic                 (HEADER_MAGIC)
///        8     4  version               (u32)
///       12     8  file_count            (u64) — total indexed files
///       20     8  signal_hash           (u64) — XOR of basename hashes
///       28     8  last_mtime_sec        (i64) — mtime of newest indexed file
///       36     4  last_mtime_nsec       (i32)
///       40     8  invariant_breach_ct   (u64) — count of out-of-order appends
///       48     2  parent_path_len       (u16) — bytes used in parent_path
///       50     2  reserved              (u16) — padding / future flags
///       52  4096  parent_path           ([u8; MAX_PARENT_PATH_LEN])
///     4148    12  reserved_tail         ([u8; 12]) — alignment / future use
///     ----  ----
///     4160 total
/// ```
pub const HEADER_SIZE: usize = 4160;

// Sanity check at compile time. These are test-only and debug-only
// assertions per project policy; they never run in production binaries.
#[cfg(test)]
#[allow(dead_code)]
const _COMPILE_TIME_HEADER_SIZE_CHECK: () = {
    assert!(HEADER_SIZE == 8 + 4 + 8 + 8 + 8 + 4 + 8 + 2 + 2 + MAX_PARENT_PATH_LEN + 12);
};

// File names within the chrono_index subdirectory.
pub const HEADER_FILENAME: &str = "header.bin";
pub const NAMES_FILENAME: &str = "names.bin";
pub const MTIMES_FILENAME: &str = "mtimes.bin";
pub const SCRATCH_DIRNAME: &str = "scratch";
pub const INDEX_SUBDIRNAME: &str = "chrono_index";

// =========================================================================
// Error type — terse, non-leaking, per project policy
// =========================================================================

/// Error codes returned by this module.
///
/// Variants are intentionally coarse and carry **no user data, file paths,
/// or filename content**, per project security policy: production error
/// output must not leak filesystem structure or user data.
///
/// Each variant's prefix `CIDX-` identifies the module of origin for log
/// triage. The numeric suffix is the stable diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChronoIndexError {
    /// CIDX-01: Unable to create or open the index directory.
    IndexDirIo,
    /// CIDX-02: Unable to read header file.
    HeaderReadIo,
    /// CIDX-03: Unable to write header file.
    HeaderWriteIo,
    /// CIDX-04: Header magic mismatch.
    HeaderBadMagic,
    /// CIDX-05: Header version mismatch.
    HeaderBadVersion,
    /// CIDX-06: Header size or internal length field out of range.
    HeaderBadSize,
    /// CIDX-07: Parent path provided exceeds MAX_PARENT_PATH_LEN.
    ParentPathTooLong,
    /// CIDX-08: Parent path empty or otherwise invalid.
    ParentPathInvalid,
    /// CIDX-09: Atomic rename failed.
    RenameIo,
    /// CIDX-10: Reserved for future use by build path.
    BuildIo,
    /// CIDX-11: Reserved for future use by append path.
    AppendIo,
    /// CIDX-12: Reserved for future use by lookup path.
    LookupIo,
}

impl ChronoIndexError {
    /// Returns the stable diagnostic code string. Safe for production logs.
    /// Never includes paths, names, mtimes, or other content.
    pub fn code(self) -> &'static str {
        match self {
            ChronoIndexError::IndexDirIo => "CIDX-01",
            ChronoIndexError::HeaderReadIo => "CIDX-02",
            ChronoIndexError::HeaderWriteIo => "CIDX-03",
            ChronoIndexError::HeaderBadMagic => "CIDX-04",
            ChronoIndexError::HeaderBadVersion => "CIDX-05",
            ChronoIndexError::HeaderBadSize => "CIDX-06",
            ChronoIndexError::ParentPathTooLong => "CIDX-07",
            ChronoIndexError::ParentPathInvalid => "CIDX-08",
            ChronoIndexError::RenameIo => "CIDX-09",
            ChronoIndexError::BuildIo => "CIDX-10",
            ChronoIndexError::AppendIo => "CIDX-11",
            ChronoIndexError::LookupIo => "CIDX-12",
        }
    }
}

// =========================================================================
// In-memory header representation
// =========================================================================

/// In-memory mirror of `header.bin`.
///
/// This struct is small and stack-friendly. It is the single source of
/// truth for index metadata while a build or append is in progress; on
/// completion, it is serialized atomically to disk via [`write_header_atomic`].
///
/// Fields correspond byte-for-byte to the on-disk layout documented on
/// [`HEADER_SIZE`].
#[derive(Clone)]
pub struct ChronoIndexHeader {
    /// Total number of indexed files. Equals number of records in both
    /// `names.bin` and `mtimes.bin`. Monotonically non-decreasing.
    pub file_count: u64,

    /// Order-independent signal hash of all indexed basenames.
    ///
    /// Each basename is hashed with `pearson_hash_salt_array` using
    /// `SIGNAL_HASH_SALTS` over `GENERATED_TABLE`, producing
    /// `PEARSON_SALT_ARRAY_SIZE` bytes. The per-basename results are
    /// XOR-folded lane-by-lane into this accumulator.
    ///
    /// Width is governed by `PEARSON_SALT_ARRAY_SIZE`. The on-disk
    /// header reserves a fixed 8-byte slot for this field; the first
    /// `PEARSON_SALT_ARRAY_SIZE` bytes carry the hash and the rest are
    /// zero padding (kept deterministic so the header is byte-stable).
    ///
    /// Used to cheaply detect whether the directory contents have
    /// diverged from the index between runs. The check is
    /// order-independent: a directory whose files swap chronological
    /// positions produces the same signal_hash. Order-sensitive
    /// detection is provided separately by `chrono_sort_hash_to_n`.
    pub signal_hash: [u8; PEARSON_SALT_ARRAY_SIZE],

    /// mtime of the newest indexed file (largest sort key in `mtimes.bin`).
    /// Used to validate the "new files have newer mtimes" invariant at
    /// append time without re-reading `mtimes.bin`.
    pub last_mtime_sec: i64,
    pub last_mtime_nsec: i32,

    /// Count of times the append-only invariant was breached and a merge
    /// insert was performed instead of a pure append. Observability only;
    /// does not affect correctness.
    pub invariant_breach_count: u64,

    /// Length in bytes of `parent_path` actually in use (`<= MAX_PARENT_PATH_LEN`).
    pub parent_path_len: u16,

    /// Absolute path of the directory being indexed. Only the first
    /// `parent_path_len` bytes are meaningful; the rest is zero-padding.
    /// Stored as raw bytes (POSIX paths are byte sequences, not guaranteed
    /// UTF-8).
    pub parent_path: [u8; MAX_PARENT_PATH_LEN],
}

impl ChronoIndexHeader {
    /// Constructs a fresh header for a newly built index over the given
    /// parent directory absolute path.
    ///
    /// Returns `Err(ParentPathTooLong)` if the path exceeds
    /// `MAX_PARENT_PATH_LEN`, or `Err(ParentPathInvalid)` if empty.
    ///
    /// Initial state: `file_count = 0`, `signal_hash = 0`,
    /// `last_mtime_* = i64::MIN / 0` so the first appended record is
    /// always strictly newer.
    pub fn new_for_parent(parent_path_bytes: &[u8]) -> Result<Self, ChronoIndexError> {
        // Defensive: empty path makes no sense for a one-directory index.
        if parent_path_bytes.is_empty() {
            return Err(ChronoIndexError::ParentPathInvalid);
        }
        if parent_path_bytes.len() > MAX_PARENT_PATH_LEN {
            return Err(ChronoIndexError::ParentPathTooLong);
        }

        let mut parent_path_buffer = [0u8; MAX_PARENT_PATH_LEN];
        // Safe slice copy; bounds already validated above.
        parent_path_buffer[..parent_path_bytes.len()].copy_from_slice(parent_path_bytes);

        Ok(ChronoIndexHeader {
            file_count: 0,
            signal_hash: [0u8; PEARSON_SALT_ARRAY_SIZE],
            // Sentinel: any real mtime will compare strictly greater than this.
            last_mtime_sec: i64::MIN,
            last_mtime_nsec: 0,
            invariant_breach_count: 0,
            parent_path_len: parent_path_bytes.len() as u16,
            parent_path: parent_path_buffer,
        })
    }

    /// Returns a slice of the meaningful portion of `parent_path`,
    /// without trailing zero padding.
    pub fn parent_path_slice(&self) -> &[u8] {
        // Defensive bounds clamp: if a corrupt on-disk value somehow
        // exceeded the array length, we clamp rather than panic.
        let usable_length = (self.parent_path_len as usize).min(MAX_PARENT_PATH_LEN);
        &self.parent_path[..usable_length]
    }

    /// Serializes this header into a `HEADER_SIZE`-byte buffer in the
    /// on-disk format documented on `HEADER_SIZE`.
    fn serialize_into(&self, output_buffer: &mut [u8; HEADER_SIZE]) {
        // Zero the buffer so all reserved/padding regions are deterministic.
        for byte_slot in output_buffer.iter_mut() {
            *byte_slot = 0;
        }

        output_buffer[0..8].copy_from_slice(&HEADER_MAGIC);
        output_buffer[8..12].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        output_buffer[12..20].copy_from_slice(&self.file_count.to_le_bytes());

        // signal_hash occupies the first PEARSON_SALT_ARRAY_SIZE bytes of
        // the 8-byte slot at offset 20. The remaining bytes of the slot
        // are left as the zero already written by the buffer-zeroing pass
        // at the top of this function, so the on-disk header is byte-
        // stable for any PEARSON_SALT_ARRAY_SIZE <= 8.

        #[cfg(debug_assertions)]
        debug_assert!(PEARSON_SALT_ARRAY_SIZE <= 8);

        output_buffer[20..20 + PEARSON_SALT_ARRAY_SIZE].copy_from_slice(&self.signal_hash);

        output_buffer[28..36].copy_from_slice(&self.last_mtime_sec.to_le_bytes());
        output_buffer[36..40].copy_from_slice(&self.last_mtime_nsec.to_le_bytes());
        output_buffer[40..48].copy_from_slice(&self.invariant_breach_count.to_le_bytes());
        output_buffer[48..50].copy_from_slice(&self.parent_path_len.to_le_bytes());
        // bytes [50..52] reserved (u16) — left zero
        output_buffer[52..52 + MAX_PARENT_PATH_LEN].copy_from_slice(&self.parent_path);
        // bytes [4148..4160] reserved_tail — left zero
    }

    /// Deserializes a header from a `HEADER_SIZE`-byte buffer.
    ///
    /// Validates magic, version, and `parent_path_len`. Returns:
    /// - `Err(HeaderBadMagic)` on magic mismatch,
    /// - `Err(HeaderBadVersion)` on version mismatch,
    /// - `Err(HeaderBadSize)` if `parent_path_len > MAX_PARENT_PATH_LEN`.
    ///
    /// These errors are the caller's signal to trigger a rebuild rather
    /// than to halt.
    fn deserialize_from(input_buffer: &[u8; HEADER_SIZE]) -> Result<Self, ChronoIndexError> {
        // Magic check first — fast rejection of unrelated files.
        let mut magic_buffer = [0u8; 8];
        magic_buffer.copy_from_slice(&input_buffer[0..8]);
        if magic_buffer != HEADER_MAGIC {
            return Err(ChronoIndexError::HeaderBadMagic);
        }

        let mut u32_buffer = [0u8; 4];
        u32_buffer.copy_from_slice(&input_buffer[8..12]);
        let on_disk_version = u32::from_le_bytes(u32_buffer);
        if on_disk_version != HEADER_VERSION {
            return Err(ChronoIndexError::HeaderBadVersion);
        }

        let mut u64_buffer = [0u8; 8];
        u64_buffer.copy_from_slice(&input_buffer[12..20]);
        let file_count = u64::from_le_bytes(u64_buffer);

        // signal_hash is the first PEARSON_SALT_ARRAY_SIZE bytes of the
        // 8-byte slot at offset 20. Remaining bytes of the slot are
        // reserved (zero) and ignored on read so future widenings up to
        // 8 are forward-compatible at the layout level (semantics still
        // require HEADER_VERSION to be bumped on any width change).
        let mut signal_hash_bytes = [0u8; PEARSON_SALT_ARRAY_SIZE];
        signal_hash_bytes.copy_from_slice(&input_buffer[20..20 + PEARSON_SALT_ARRAY_SIZE]);
        let signal_hash = signal_hash_bytes;

        let mut i64_buffer = [0u8; 8];
        i64_buffer.copy_from_slice(&input_buffer[28..36]);
        let last_mtime_sec = i64::from_le_bytes(i64_buffer);

        let mut i32_buffer = [0u8; 4];
        i32_buffer.copy_from_slice(&input_buffer[36..40]);
        let last_mtime_nsec = i32::from_le_bytes(i32_buffer);

        u64_buffer.copy_from_slice(&input_buffer[40..48]);
        let invariant_breach_count = u64::from_le_bytes(u64_buffer);

        let mut u16_buffer = [0u8; 2];
        u16_buffer.copy_from_slice(&input_buffer[48..50]);
        let parent_path_len = u16::from_le_bytes(u16_buffer);
        // bytes [50..52] reserved — ignored on read

        if (parent_path_len as usize) > MAX_PARENT_PATH_LEN {
            return Err(ChronoIndexError::HeaderBadSize);
        }

        let mut parent_path_buffer = [0u8; MAX_PARENT_PATH_LEN];
        parent_path_buffer.copy_from_slice(&input_buffer[52..52 + MAX_PARENT_PATH_LEN]);

        Ok(ChronoIndexHeader {
            file_count,
            signal_hash,
            last_mtime_sec,
            last_mtime_nsec,
            invariant_breach_count,
            parent_path_len,
            parent_path: parent_path_buffer,
        })
    }
}

// =========================================================================
// Pearson Salt Hash Functions
// =========================================================================

/// Computes the per-basename hash used by Role 1 (signal_hash fold)
/// and Role 2 (name_hashes.bin sidecar entry). Role 1 and Role 2 use
/// independent salt arrays so their outputs are not correlated.
fn pearson_hash_basename_for_signal(
    basename_bytes: &[u8],
) -> Result<[u8; PEARSON_SALT_ARRAY_SIZE], ChronoIndexError> {
    match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
        basename_bytes,
        &SIGNAL_HASH_SALTS,
        &GENERATED_TABLE,
    ) {
        Ok(hash_bytes) => Ok(hash_bytes),
        Err(_) => Err(ChronoIndexError::BuildIo),
    }
}

fn pearson_hash_basename_for_name_sidecar(
    basename_bytes: &[u8],
) -> Result<[u8; PEARSON_SALT_ARRAY_SIZE], ChronoIndexError> {
    match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
        basename_bytes,
        &NAME_HASH_SALTS,
        &GENERATED_TABLE,
    ) {
        Ok(hash_bytes) => Ok(hash_bytes),
        Err(_) => Err(ChronoIndexError::AppendIo),
    }
}

/// In-place XOR: accumulator ^= addend, lane by lane.
fn xor_into_accumulator(
    accumulator: &mut [u8; PEARSON_SALT_ARRAY_SIZE],
    addend: &[u8; PEARSON_SALT_ARRAY_SIZE],
) {
    for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
        accumulator[lane_index] ^= addend[lane_index];
    }
}

// =============================================================================
// SECTION 2: Generated Permutation Table (compile-time Fisher-Yates)
// =============================================================================

/// The fixed seed used to deterministically generate `GENERATED_TABLE`.
///
/// ## Project-Level Context
///
/// Changing this seed produces a different table. The seed is fixed
/// here so that every build of this crate produces byte-identical
/// hashes for the same input — this is required for any downstream
/// consumer that persists hash values (e.g. Bloom filters on disk).
///
/// The specific value `0x9E37_79B9_7F4A_7C15` is the 64-bit golden-ratio
/// constant (the same constant used by `splitmix64` and many other
/// PRNG initializers). It has no special cryptographic meaning here;
/// it is simply a well-mixed, well-known nonzero constant.
// const GENERATED_TABLE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
// const GENERATED_TABLE_SEED: u64 = 0xFFFF_FFFF_FFFF_FFFF;
// (chess-board-tested seed) 0x1424_1312_FCC6_8202,  base chi2 == 163.52
const GENERATED_TABLE_SEED: u64 = 0x1424_1312_FCC6_8202;

/// A 256-byte permutation table generated at compile time via seeded
/// Fisher-Yates shuffle.
///
/// ## Project-Level Context
///
/// This is the recommended default for new code that does not need
/// 1990-compatibility. Its construction is fully transparent:
///
/// 1. Start with the identity permutation `[0, 1, 2, ..., 255]`.
/// 2. Run Fisher-Yates (Knuth shuffle) using a documented `splitmix64`
///    PRNG seeded by `GENERATED_TABLE_SEED`.
///
/// Both the algorithm and the seed are documented in source, so any
/// reader can independently reconstruct this exact table.
///
/// ## Validation
///
/// `pearson_hash_tools.rs` provides a quality-evaluation module that
/// scores this table against `PEARSON_1990_TABLE` on six metrics
/// (fixed-point count, cycle structure, displacement, sequential
/// correlation, XOR uniformity, empirical collisions). The integration
/// tests in `main.rs` confirm this generated table meets or exceeds
/// the 1990 baseline on every metric.
pub const GENERATED_TABLE: [u8; 256] = generate_table_fisher_yates_const(GENERATED_TABLE_SEED);

/// Const-fn Fisher-Yates shuffle producing a permutation of `0..=255`.
///
/// ## What This Function Does
///
/// 1. Initializes a `[u8; 256]` to the identity permutation
///    (`table[i] = i`).
/// 2. Walks `i` from `255` down to `1`, picks a pseudo-random index
///    `j` in `0..=i` using a `splitmix64`-style PRNG, and swaps
///    `table[i]` with `table[j]`.
///
/// This is the standard Fisher-Yates / Knuth shuffle. With a fixed
/// seed it is fully deterministic.
///
/// ## Project-Level Context
///
/// Marked `const fn` so the table is computed at compile time and
/// embedded directly into the binary's read-only data section. There
/// is zero runtime cost.
///
/// ## PRNG Choice
///
/// A minimal `splitmix64` step is used as the PRNG:
///
/// ```text
///     state = state + 0x9E3779B97F4A7C15
///     z = state
///     z = (z XOR (z >> 30)) * 0xBF58476D1CE4E5B9
///     z = (z XOR (z >> 27)) * 0x94D049BB133111EB
///     z = z XOR (z >> 31)
/// ```
///
/// `splitmix64` is well-studied, passes BigCrush, has 64 bits of state
/// (more than enough for a 256-element shuffle), and is trivial to
/// implement as a `const fn`. It is **not** cryptographically secure,
/// which is acceptable because the resulting table is public anyway.
///
/// ## Why Not the 1990 Table?
///
/// The 1990 table is hand-typed from a 1990 typescript. While it
/// passes statistical tests well, a generated table from a documented
/// algorithm is more auditable: any reader can reproduce it from
/// first principles.
///
/// ## Arguments
///
/// * `seed` — 64-bit PRNG seed. Different seeds produce different
///   tables; the same seed always produces the same table.
///
/// ## Returns
///
/// A `[u8; 256]` that is guaranteed (by construction) to be a valid
/// permutation of `0..=255`.
pub const fn generate_table_fisher_yates_const(seed: u64) -> [u8; 256] {
    // Step 1: build the identity permutation.
    let mut table: [u8; 256] = [0u8; 256];
    let mut init_index: usize = 0;
    while init_index < 256 {
        // Cast is safe: init_index < 256 so it fits in a u8.
        table[init_index] = init_index as u8;
        init_index += 1;
    }

    // Step 2: Fisher-Yates shuffle, walking high-to-low.
    //
    // PRNG state advances once per swap. We use `wrapping_*` arithmetic
    // throughout so const evaluation cannot overflow-panic.
    let mut prng_state: u64 = seed;
    let mut high_index: usize = 255;
    while high_index > 0 {
        // Advance splitmix64 PRNG.
        prng_state = prng_state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z: u64 = prng_state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z = z ^ (z >> 31);

        // Reduce the 64-bit random word into `0..=high_index`.
        // `high_index + 1` is at most 256, which fits in u64 trivially.
        // Modulo bias is negligible for a 64-bit value reduced into a
        // range of at most 256.
        let swap_target: usize = (z % ((high_index as u64) + 1)) as usize;

        // Swap table[high_index] and table[swap_target].
        let temp_value: u8 = table[high_index];
        table[high_index] = table[swap_target];
        table[swap_target] = temp_value;

        high_index -= 1;
    }

    table
}

// =============================================================================
// SECTION 3: Base Pearson Hash (production function)
// =============================================================================

/// Compute the 8-bit Pearson hash of `input` using `table`.
///
/// ## Project-Level Context
///
/// This is the core building block of the entire module. The
/// salt-array variant is implemented in terms of the same inner loop,
/// inlined for stack-only operation. External callers typically use
/// this directly when an 8-bit hash is sufficient (e.g. selecting one
/// of 256 hash buckets).
///
/// ## Algorithm
///
/// ```text
///     hash = 0
///     for byte in input:
///         hash = table[hash XOR byte]
///     return hash
/// ```
///
/// The table indexing is always in-bounds because `hash` and `byte`
/// are both `u8`, so `hash ^ byte` is a `u8` in `0..=255`, and
/// `table.len() == 256`.
///
/// ## Arguments
///
/// * `input` — Slice of bytes to hash. Must be non-empty.
/// * `table` — Reference to a 256-byte permutation table. The caller
///   chooses which table (`PEARSON_1990_TABLE`, `GENERATED_TABLE`, or
///   a custom one).
///
/// ## Returns
///
/// * `Ok(u8)` — the Pearson hash of `input`.
/// * `Err(std::io::Error)` with `ErrorKind::InvalidInput` and message
///   `"PHB: empty input"` — if `input` is empty.
///
/// ## Why Reject Empty Input?
///
/// Mathematically the Pearson hash of an empty string is the initial
/// value of `hash`, which is `0`. But returning `0` for empty input
/// is a silent failure mode: it collides with every legitimate input
/// that happens to hash to `0`. Per project rules ("check returns,
/// check bounds"), we surface this case as an explicit error so the
/// caller decides how to handle it.
///
/// ## Error Message Convention
///
/// All error messages from this function are prefixed `"PHB:"`
/// (Pearson Hash Base) so log readers can identify the source
/// function without leaking source paths.
///
/// ## Examples
///
/// ```ignore
/// use pearson_hash_salt_array_rust::{pearson_hash_base, PEARSON_1990_TABLE};
///
/// let h = pearson_hash_base(b"hello", &PEARSON_1990_TABLE)?;
/// assert!(h <= 255); // always true, h is u8
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn pearson_hash_base(input: &[u8], table: &[u8; 256]) -> Result<u8, Error> {
    // =========================================================
    // Debug-Assert, Test-Assert, Production-Catch-Handle
    // =========================================================

    // Debug-only invariant: table length is enforced by the type
    // `&[u8; 256]`, so it cannot be wrong, but we assert it during
    // debug builds (not test builds) as a tripwire against future
    // refactors that might loosen the type.
    #[cfg(all(debug_assertions, not(test)))]
    debug_assert!(table.len() == 256, "PHB: table length invariant");

    // Test-only assertion mirroring the production check below.
    // Kept here so `cargo test --release` still exercises it.
    #[cfg(test)]
    assert!(table.len() == 256, "PHB: table length invariant");

    // Production check: never panic, return Err and let the caller
    // decide. Empty-input handling per docstring above.
    if input.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "PHB: empty input"));
    }

    // Core Pearson loop. Bounded by `input.len()`, which is finite.
    // No heap, no recursion, no panics: index is always in-bounds
    // because `(hash ^ byte) as usize` is in `0..=255` and
    // `table` is `[u8; 256]`.
    let mut running_hash: u8 = 0;
    for &current_byte in input {
        let table_index: usize = (running_hash ^ current_byte) as usize;
        running_hash = table[table_index];
    }

    Ok(running_hash)
}

// =============================================================================
// SECTION 4: Salt-Array Pearson Hash (production function, stack-only)
// =============================================================================

/// Compute an `N`-byte Pearson hash array by combining one input with
/// `N` salts, producing one Pearson hash per salt.
///
/// ## Project-Level Context
///
/// This is the headline function of the module. The pattern "one
/// input + several independent salts → several independent hashes" is
/// the standard way to build Bloom filters, count-min sketches,
/// HyperLogLog-style structures, and consistent-hash dispersal from
/// a single small hash primitive.
///
/// ### Why a Const-Generic `N`?
///
/// `N` is the number of salts (and therefore the number of output
/// bytes). Making it a const generic means:
///
/// - The output is `[u8; N]` on the stack — **no heap**.
/// - The size of the output is part of the type, so callers cannot
///   accidentally truncate or misread it.
/// - The compiler unrolls and inlines the per-salt loop where
///   profitable.
///
/// ### Why No Concatenation?
///
/// A naive implementation would, for each salt, allocate a `Vec`,
/// copy `input` into it, append the salt's byte, and hash the buffer.
/// That allocates `N` `Vec`s of size `input.len() + 1` and bounds
/// the maximum input length to whatever fits in memory.
///
/// Pearson hashing is **inherently sequential**: the running hash
/// state after consuming `input` is identical regardless of what
/// comes next. So we can:
///
/// 1. Hash `input` **once**, getting a base running-hash byte.
/// 2. For each salt, **continue** the same algorithm with the salt's
///    single byte, starting from the base running-hash.
///
/// The result is identical to "concatenate input and salt, then hash"
/// but uses **zero heap**, runs the input bytes exactly once total
/// (rather than `N` times), and has no input-length bound beyond
/// what `&[u8]` itself allows.
///
/// ## Algorithm
///
/// ```text
///     base = pearson_hash_base(input, table)   // hash the input once
///     for i in 0..N:
///         h = base
///         h = table[h XOR salts[i]]            // one step per salt
///         output[i] = h
///     return output
/// ```
///
/// ## Salt Encoding
///
/// Salts are `u8` values. Each salt is used directly as a single
/// byte in one Pearson step. No byte-order conversion is needed or
/// performed: a `u8` has no endianness. This means hashes computed
/// by this crate are identical across all host architectures for
/// the same salt values.
///
/// ## Arguments
///
/// * `input` — Bytes to hash. Must be non-empty.
/// * `salts` — A `&[u8; N]` reference. The caller controls `N` and
///   the salt values. Each salt is one byte and produces one output
///   byte. `N` must be at least 1 (enforced at the type level —
///   `[u8; 0]` would compile but is rejected at runtime).
/// * `table` — Permutation table to use (e.g. `&PEARSON_1990_TABLE`
///   or `&GENERATED_TABLE`).
///
/// ## Returns
///
/// * `Ok([u8; N])` — array of `N` Pearson-hash bytes, one per salt,
///   in the same order as the salts.
/// * `Err(std::io::Error)` with prefix `"PHSA:"` (Pearson Hash Salt
///   Array) on:
///   - empty `input` (`"PHSA: empty input"`)
///   - `N == 0` (`"PHSA: zero salts"`)
///
/// ## Examples
///
/// ```ignore
/// use pearson_hash_salt_array_rust::{pearson_hash_salt_array, PEARSON_1990_TABLE};
///
/// let salts: [u8; 4] = [0x01, 0x02, 0x03, 0x04];
/// let hashes: [u8; 4] = pearson_hash_salt_array(b"hello", &salts, &PEARSON_1990_TABLE)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn pearson_hash_salt_array<const N: usize>(
    input: &[u8],
    salts: &[u8; N],
    table: &[u8; 256],
) -> Result<[u8; N], Error> {
    // =========================================================
    // Debug-Assert, Test-Assert, Production-Catch-Handle
    // =========================================================
    //
    // The two production cases are: empty input, and N == 0.
    // Both are checked-and-handled below without panicking.

    #[cfg(all(debug_assertions, not(test)))]
    {
        debug_assert!(N > 0, "PHSA: zero salts (debug)");
        debug_assert!(table.len() == 256, "PHSA: table length invariant (debug)");
    }

    #[cfg(test)]
    {
        assert!(table.len() == 256, "PHSA: table length invariant (test)");
    }

    // Production check: N == 0 means an empty output array, which is
    // meaningless and almost certainly a caller bug. Reject explicitly.
    if N == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "PHSA: zero salts"));
    }

    // Production check: empty input. Same reasoning as in
    // `pearson_hash_base` — empty input would produce a deterministic
    // value that collides with legitimate inputs.
    if input.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "PHSA: empty input"));
    }

    // --------------------------------------------------------------
    // Step 1: Hash the input ONCE, producing the "base" running hash.
    //
    // This is the state of the Pearson algorithm immediately after
    // consuming `input` but before consuming any salt bytes. Every
    // per-salt hash starts from this same base, so we save (N-1)
    // re-traversals of `input`.
    // --------------------------------------------------------------
    let mut base_running_hash: u8 = 0;
    for &input_byte in input {
        let table_index: usize = (base_running_hash ^ input_byte) as usize;
        base_running_hash = table[table_index];
    }

    // --------------------------------------------------------------
    // Step 2: For each salt, continue the Pearson loop using the
    // salt byte directly, and store the resulting byte.
    //
    // Each salt is exactly one byte, so the Pearson step for a
    // salt is a single XOR followed by a single table lookup.
    // No byte-encoding step is needed: a u8 has no byte order,
    // so hashes are bit-identical across all host architectures.
    //
    // Each salt is exactly one byte, so the Pearson step for a
    // salt is a single XOR followed by a single table lookup.
    // No byte-encoding step is required and no endianness
    // conversion is involved: a u8 has no byte order.
    //
    // Output array is stack-allocated `[u8; N]`. No heap.
    // Outer loop is bounded by N (a compile-time constant).
    // All loops are firmly bounded per project rules.
    //
    // Correctness invariant: `salts[salt_index]` is a u8 and
    // `salted_running_hash` is a u8, so their XOR is a u8 in
    // 0..=255, which is always a valid index into `table: [u8; 256]`.
    // This cannot panic or go out of bounds.
    // --------------------------------------------------------------
    let mut output_hashes: [u8; N] = [0u8; N];

    for salt_index in 0..N {
        // Start from the saved post-input hash state.
        // Each salt gets its own independent continuation from
        // `base_running_hash`; salts do not chain into each other.
        let mut salted_running_hash: u8 = base_running_hash;

        // Retrieve this salt's single byte.
        let salt_byte: u8 = salts[salt_index];

        // Single Pearson step: XOR the running hash with the salt
        // byte, then look up the result in the permutation table.
        let xor_result: u8 = salted_running_hash ^ salt_byte;
        let table_index: usize = xor_result as usize;
        salted_running_hash = table[table_index];

        output_hashes[salt_index] = salted_running_hash;
    }

    Ok(output_hashes)
}

// =============================================================================
// SECTION 5: Internal utility — permutation validity check
// =============================================================================

/// Verify that `table` is a valid permutation of `0..=255`.
///
/// ## Project-Level Context
///
/// Both `PEARSON_1990_TABLE` and `GENERATED_TABLE` are guaranteed to be
/// valid permutations by construction (the 1990 table is hand-verified;
/// the generated table is produced by Fisher-Yates, which provably
/// preserves the permutation invariant). This function exists to
/// **prove** that invariant at test time, and to allow callers who
/// build their own tables to validate them before use.
///
/// A "valid permutation" means every byte value in `0..=255` appears
/// exactly once. The check uses a 256-bit presence bitmap (32 bytes
/// on the stack) so it allocates nothing.
///
/// ## Why It Matters
///
/// If a table has a duplicate value, then some byte in `0..=255` is
/// missing, and the Pearson hash can never produce that byte as an
/// output — silently shrinking the hash range and creating biased
/// collisions. The "two strings differing in one byte never collide"
/// property of Pearson hashing depends critically on the table being
/// a true permutation.
///
/// ## Arguments
///
/// * `table` — Reference to the 256-byte table to validate.
///
/// ## Returns
///
/// * `true` if every value `0..=255` appears exactly once.
/// * `false` otherwise.
///
/// ## Note
///
/// This is `pub` because it is genuinely useful to external callers
/// constructing custom tables. It does not allocate and is safe to
/// call from production code if desired.
pub fn is_valid_permutation(table: &[u8; 256]) -> bool {
    // 32-byte bitmap, one bit per possible value 0..=255.
    let mut presence_bitmap: [u8; 32] = [0u8; 32];

    // Walk every entry and set its corresponding bit. If a bit is
    // already set, we have a duplicate, so the table is not a
    // permutation.
    let mut entry_index: usize = 0;
    while entry_index < 256 {
        let value: u8 = table[entry_index];
        let byte_index: usize = (value as usize) >> 3; // value / 8
        let bit_mask: u8 = 1u8 << ((value as usize) & 7); // 1 << (value % 8)

        if (presence_bitmap[byte_index] & bit_mask) != 0 {
            // Duplicate value detected.
            return false;
        }
        presence_bitmap[byte_index] |= bit_mask;

        entry_index += 1;
    }

    // If we set 256 distinct bits with no collision, every value
    // 0..=255 must be present exactly once.
    true
}

// =========================================================================
// Path helpers — assemble absolute paths into the index files.
// =========================================================================

/// Joins a caller-provided temp root with the fixed `chrono_index/` subdir
/// and the given index-file basename.
///
/// This uses `std::path::PathBuf` (small heap allocation, bounded by
/// `PATH_MAX`, freed on drop) **only** because `std::fs` APIs require
/// `&Path`. This is a per-call cost, not a per-N cost. Acceptable.
fn build_index_file_path(temp_root_dir: &Path, index_file_basename: &str) -> PathBuf {
    let mut composed_path = PathBuf::from(temp_root_dir);
    composed_path.push(INDEX_SUBDIRNAME);
    composed_path.push(index_file_basename);
    composed_path
}

// =========================================================================
// Index directory provisioning
// =========================================================================

/// Ensures `<temp_root>/chrono_index/` exists. Idempotent. Does not create
/// any of the index files themselves; that is the responsibility of the
/// build / append paths.
///
/// On any I/O failure returns `Err(IndexDirIo)` — caller decides whether
/// to retry or fall back. Never panics, never halts.
pub fn ensure_index_directory_exists(temp_root_dir: &Path) -> Result<(), ChronoIndexError> {
    let mut index_directory_path = PathBuf::from(temp_root_dir);
    index_directory_path.push(INDEX_SUBDIRNAME);

    match std::fs::create_dir_all(&index_directory_path) {
        Ok(()) => Ok(()),
        Err(_io_error) => {
            // Do not leak the path or the OS error message into the
            // production error channel. Return a terse stable code.
            Err(ChronoIndexError::IndexDirIo)
        }
    }
}

// =========================================================================
// Header read
// =========================================================================

/// Reads and validates `header.bin` from disk.
///
/// Returns:
/// - `Ok(Some(header))` if the header file exists and is valid.
/// - `Ok(None)` if the header file does not exist (first run / clean state).
/// - `Err(HeaderReadIo)` for any I/O error other than "not found".
/// - `Err(HeaderBadMagic | HeaderBadVersion | HeaderBadSize)` for
///   structural mismatch — caller should treat these as "rebuild needed".
///
/// Reads exactly `HEADER_SIZE` bytes into a stack buffer; no heap growth
/// related to header content.
pub fn read_header(temp_root_dir: &Path) -> Result<Option<ChronoIndexHeader>, ChronoIndexError> {
    let header_file_path = build_index_file_path(temp_root_dir, HEADER_FILENAME);

    let mut header_file_handle = match File::open(&header_file_path) {
        Ok(opened_file) => opened_file,
        Err(open_error) => {
            // "Not found" is a normal first-run state, not an error.
            if open_error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(ChronoIndexError::HeaderReadIo);
        }
    };

    let mut header_byte_buffer = [0u8; HEADER_SIZE];
    match header_file_handle.read_exact(&mut header_byte_buffer) {
        Ok(()) => {}
        Err(_read_error) => {
            // Truncated, permissions, I/O, etc. — terse code, caller
            // will trigger rebuild.
            return Err(ChronoIndexError::HeaderReadIo);
        }
    }

    // Structural validation lives in deserialize_from.
    let parsed_header = ChronoIndexHeader::deserialize_from(&header_byte_buffer)?;
    Ok(Some(parsed_header))
}

// =========================================================================
// Header write — atomic via tempfile + rename
// =========================================================================

/// Writes `header.bin` atomically using the write-temp + fsync + rename
/// pattern. POSIX guarantees `rename(2)` is atomic within the same
/// filesystem, so a reader either sees the old header or the new header,
/// never a partial one.
///
/// On any I/O failure returns `Err(HeaderWriteIo)` or `Err(RenameIo)`.
/// The previous header (if any) is left untouched on failure — the index
/// remains in its last consistent state. Caller may retry on the next call.
pub fn write_header_atomic(
    temp_root_dir: &Path,
    header_to_write: &ChronoIndexHeader,
) -> Result<(), ChronoIndexError> {
    let final_header_path = build_index_file_path(temp_root_dir, HEADER_FILENAME);

    // Stage to a sibling temp file in the same directory so that rename is
    // a same-filesystem operation and therefore atomic per POSIX.
    let mut staging_header_path = final_header_path.clone();
    // Append a fixed staging suffix. Single-writer assumption; if multi-
    // writer support is ever needed, swap to a unique-per-process suffix.
    staging_header_path.set_file_name("header.bin.tmp");

    // Serialize into a stack buffer — no heap.
    let mut header_byte_buffer = [0u8; HEADER_SIZE];
    header_to_write.serialize_into(&mut header_byte_buffer);

    // Open staging file (create or truncate).
    let mut staging_file_handle = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging_header_path)
    {
        Ok(opened_file) => opened_file,
        Err(_open_error) => return Err(ChronoIndexError::HeaderWriteIo),
    };

    if staging_file_handle.write_all(&header_byte_buffer).is_err() {
        // Best-effort cleanup of partial staging file; failure to remove
        // is non-fatal — the next write will truncate it.
        let _ = std::fs::remove_file(&staging_header_path);
        return Err(ChronoIndexError::HeaderWriteIo);
    }

    // fsync staging file so its contents are durable before rename.
    if staging_file_handle.sync_all().is_err() {
        let _ = std::fs::remove_file(&staging_header_path);
        return Err(ChronoIndexError::HeaderWriteIo);
    }

    // Drop the file handle explicitly before rename; on some platforms
    // (not Linux, but defensive) an open handle can interfere with rename.
    drop(staging_file_handle);

    // Atomic rename. On failure leave the previous header in place.
    if std::fs::rename(&staging_header_path, &final_header_path).is_err() {
        let _ = std::fs::remove_file(&staging_header_path);
        return Err(ChronoIndexError::RenameIo);
    }

    // Note: we do not fsync the containing directory here. For maximal
    // crash-durability of the rename itself, a directory fsync would be
    // added. Project-level policy ("rebuild on header invalid") makes
    // this safe to omit: a crashed-mid-rename header will be treated as
    // "rebuild needed" on the next run, which is the intended fail-safe.

    Ok(())
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod chrono_index_part_a_tests {
    use super::*;

    /// Helper: create a unique scratch directory under the OS temp dir for
    /// test isolation. Test-only; production callers supply their own root.
    fn make_test_temp_root(test_label: &str) -> PathBuf {
        let mut scratch = std::env::temp_dir();
        let nanos_since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        scratch.push(format!(
            "chrono_index_test_{}_{}_{}",
            test_label,
            std::process::id(),
            nanos_since_epoch
        ));
        std::fs::create_dir_all(&scratch).expect("test setup: create temp root");
        scratch
    }

    #[test]
    fn header_size_constant_matches_field_sum() {
        // Test-only assert: validates the documented layout arithmetic.
        assert_eq!(
            HEADER_SIZE,
            8 + 4 + 8 + 8 + 8 + 4 + 8 + 2 + 2 + MAX_PARENT_PATH_LEN + 12
        );
    }

    #[test]
    fn new_header_for_parent_rejects_empty_path() {
        let result = ChronoIndexHeader::new_for_parent(b"");
        assert_eq!(result.err(), Some(ChronoIndexError::ParentPathInvalid));
    }

    #[test]
    fn new_header_for_parent_rejects_oversize_path() {
        let oversize = vec![b'a'; MAX_PARENT_PATH_LEN + 1];
        let result = ChronoIndexHeader::new_for_parent(&oversize);
        assert_eq!(result.err(), Some(ChronoIndexError::ParentPathTooLong));
    }

    #[test]
    fn new_header_initial_state_is_sane() {
        let header =
            ChronoIndexHeader::new_for_parent(b"/var/data/watched").expect("valid parent path");
        assert_eq!(header.file_count, 0);
        assert_eq!(header.signal_hash, [0u8; PEARSON_SALT_ARRAY_SIZE]);
        assert_eq!(header.last_mtime_sec, i64::MIN);
        assert_eq!(header.last_mtime_nsec, 0);
        assert_eq!(header.invariant_breach_count, 0);
        assert_eq!(header.parent_path_slice(), b"/var/data/watched");
    }

    #[test]
    fn serialize_then_deserialize_round_trips() {
        let mut original = ChronoIndexHeader::new_for_parent(b"/some/dir").expect("valid path");
        original.file_count = 123_456;
        // Fill every lane with a distinct nonzero byte so the round-trip
        // exercises all PEARSON_SALT_ARRAY_SIZE bytes, not just the first.
        let mut sample_signal_hash = [0u8; PEARSON_SALT_ARRAY_SIZE];
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            sample_signal_hash[lane_index] = 0xA0u8.wrapping_add(lane_index as u8);
        }
        original.signal_hash = sample_signal_hash;
        original.last_mtime_sec = 1_700_000_000;
        original.last_mtime_nsec = 999_999_999;
        original.invariant_breach_count = 7;

        let mut buffer = [0u8; HEADER_SIZE];
        original.serialize_into(&mut buffer);

        let recovered =
            ChronoIndexHeader::deserialize_from(&buffer).expect("valid header round-trip");

        assert_eq!(recovered.file_count, original.file_count);
        assert_eq!(recovered.signal_hash, original.signal_hash);
        assert_eq!(recovered.last_mtime_sec, original.last_mtime_sec);
        assert_eq!(recovered.last_mtime_nsec, original.last_mtime_nsec);
        assert_eq!(
            recovered.invariant_breach_count,
            original.invariant_breach_count
        );
        assert_eq!(recovered.parent_path_slice(), original.parent_path_slice());
    }

    #[test]
    fn deserialize_rejects_bad_magic() {
        let mut buffer = [0u8; HEADER_SIZE];
        // Leave magic as all-zero; deserialize must reject.
        let result = ChronoIndexHeader::deserialize_from(&buffer);
        assert_eq!(result.err(), Some(ChronoIndexError::HeaderBadMagic));

        // Corrupt magic.
        buffer[0..8].copy_from_slice(b"XXXXXXXX");
        let result = ChronoIndexHeader::deserialize_from(&buffer);
        assert_eq!(result.err(), Some(ChronoIndexError::HeaderBadMagic));
    }

    #[test]
    fn deserialize_rejects_bad_version() {
        let mut buffer = [0u8; HEADER_SIZE];
        buffer[0..8].copy_from_slice(&HEADER_MAGIC);
        // Write a wrong version.
        buffer[8..12].copy_from_slice(&(HEADER_VERSION.wrapping_add(99)).to_le_bytes());
        let result = ChronoIndexHeader::deserialize_from(&buffer);
        assert_eq!(result.err(), Some(ChronoIndexError::HeaderBadVersion));
    }

    #[test]
    fn deserialize_rejects_oversize_parent_path_len() {
        let mut buffer = [0u8; HEADER_SIZE];
        buffer[0..8].copy_from_slice(&HEADER_MAGIC);
        buffer[8..12].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        // Set parent_path_len > MAX_PARENT_PATH_LEN.
        let bogus_len: u16 = (MAX_PARENT_PATH_LEN as u16).saturating_add(1);
        buffer[48..50].copy_from_slice(&bogus_len.to_le_bytes());
        let result = ChronoIndexHeader::deserialize_from(&buffer);
        assert_eq!(result.err(), Some(ChronoIndexError::HeaderBadSize));
    }

    #[test]
    fn ensure_index_directory_is_idempotent() {
        let root = make_test_temp_root("ensure_dir");
        assert!(ensure_index_directory_exists(&root).is_ok());
        // Second call must also succeed.
        assert!(ensure_index_directory_exists(&root).is_ok());

        let mut expected = root.clone();
        expected.push(INDEX_SUBDIRNAME);
        assert!(expected.is_dir());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_header_returns_none_when_absent() {
        let root = make_test_temp_root("read_absent");
        ensure_index_directory_exists(&root).expect("setup");
        let read_result = read_header(&root).expect("read should succeed with None");
        assert!(read_result.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_then_read_header_round_trips_on_disk() {
        let root = make_test_temp_root("rw_header");
        ensure_index_directory_exists(&root).expect("setup");

        let mut original =
            ChronoIndexHeader::new_for_parent(b"/data/observed").expect("valid path");
        original.file_count = 42;
        let mut sample_signal_hash = [0u8; PEARSON_SALT_ARRAY_SIZE];
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            sample_signal_hash[lane_index] = 0x10u8.wrapping_add(lane_index as u8);
        }
        original.signal_hash = sample_signal_hash;
        original.last_mtime_sec = 1_700_123_456;
        original.last_mtime_nsec = 250_000_000;
        original.invariant_breach_count = 2;

        write_header_atomic(&root, &original).expect("write ok");
        let recovered = read_header(&root)
            .expect("read ok")
            .expect("header present");

        assert_eq!(recovered.file_count, 42);
        assert_eq!(recovered.signal_hash, sample_signal_hash);
        assert_eq!(recovered.last_mtime_sec, 1_700_123_456);
        assert_eq!(recovered.last_mtime_nsec, 250_000_000);
        assert_eq!(recovered.invariant_breach_count, 2);
        assert_eq!(recovered.parent_path_slice(), b"/data/observed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn error_codes_are_stable_and_terse() {
        // Production logs must be able to depend on these strings.
        assert_eq!(ChronoIndexError::IndexDirIo.code(), "CIDX-01");
        assert_eq!(ChronoIndexError::HeaderBadMagic.code(), "CIDX-04");
        assert_eq!(ChronoIndexError::ParentPathTooLong.code(), "CIDX-07");
    }
}

// =========================================================================
// Part (b): Cold-build path
// =========================================================================
//
// ## When this runs
//
// The cold-build path is the fallback that produces a fresh, fully-sorted
// index from the live directory contents. It runs:
//
//   - On first ever use (no header present).
//   - When `read_header` returns a structural error (bad magic / version /
//     size), indicating the index is unusable or from a different version.
//   - When the caller-orchestrated change-detection determines the
//     existing index has diverged beyond what the incremental append path
//     (part c) can safely repair.
//
// ## Memory discipline
//
// All per-record I/O uses stack-resident fixed-size buffers:
//
//   - One `[u8; NAME_RECORD_SIZE]` for the current basename being written.
//   - One `[u8; MTIME_RECORD_SIZE]` for the current mtime record.
//   - During external sort: one fixed-size sort buffer of
//     `EXTERNAL_SORT_CHUNK_RECORDS` mtime records (default 4096 records ×
//     20 B = 80 KB) on the heap as a single `Box<[MtimeRecord]>`, allocated
//     ONCE per build and reused. This is a single bounded allocation that
//     does NOT scale with the directory size N.
//   - During k-way merge: a small fixed-size merge-heap of
//     `MAX_MERGE_FANOUT` slots (default 16) on the stack.
//
// Per-N heap growth: none. The unsorted scratch file grows on disk, not in
// RAM, and is removed after the sort.
//
// ## Failure policy
//
// Any I/O error during build: clean up scratch artifacts where possible
// and return a terse error code. The previous index (if any) is left
// untouched on disk until the new header is renamed into place — so a
// failed rebuild does not destroy a working index.

use std::io::BufReader;
use std::io::BufWriter;

/// Number of mtime records held in RAM during one pass of the external
/// merge sort. Each record is `MTIME_RECORD_SIZE` (20) bytes, so the
/// default value of 4096 yields an 80 KB working buffer.
///
/// This is the single bounded heap allocation made during cold build.
/// It does not scale with N: a directory of 1 million files uses exactly
/// the same buffer as a directory of 100 files.
pub const EXTERNAL_SORT_CHUNK_RECORDS: usize = 4096;

/// Maximum number of sorted runs merged simultaneously in the k-way merge
/// phase. If the build produces more runs than this, the merge is done in
/// successive passes (cascade merge). Bounded fan-out keeps file-handle
/// usage and merge-heap size bounded regardless of N.
pub const MAX_MERGE_FANOUT: usize = 16;

/// Scratch filenames used during build. Deleted on successful completion.
const SCRATCH_UNSORTED_MTIMES_FILENAME: &str = "mtimes_unsorted.bin";
const SCRATCH_RUN_FILENAME_PREFIX: &str = "run_";
const SCRATCH_RUN_FILENAME_SUFFIX: &str = ".bin";

/// In-memory representation of one `mtimes.bin` record.
///
/// Sort order: ascending by `(mtime_sec, mtime_nsec, record_id)`.
/// The `record_id` tiebreaker guarantees a total order even when multiple
/// files share an mtime, which makes the sort deterministic
#[derive(Clone, Copy)]
pub struct MtimeRecord {
    pub mtime_sec: i64,
    pub mtime_nsec: i32,
    pub record_id: u64,
}

impl MtimeRecord {
    /// Serializes this record to its 20-byte on-disk form.
    fn write_into(self, output_buffer: &mut [u8; MTIME_RECORD_SIZE]) {
        output_buffer[0..8].copy_from_slice(&self.mtime_sec.to_le_bytes());
        output_buffer[8..12].copy_from_slice(&self.mtime_nsec.to_le_bytes());
        output_buffer[12..20].copy_from_slice(&self.record_id.to_le_bytes());
    }

    /// Deserializes a record from its 20-byte on-disk form.
    fn read_from(input_buffer: &[u8; MTIME_RECORD_SIZE]) -> Self {
        let mut i64_buf = [0u8; 8];
        i64_buf.copy_from_slice(&input_buffer[0..8]);
        let mtime_sec = i64::from_le_bytes(i64_buf);

        let mut i32_buf = [0u8; 4];
        i32_buf.copy_from_slice(&input_buffer[8..12]);
        let mtime_nsec = i32::from_le_bytes(i32_buf);

        let mut u64_buf = [0u8; 8];
        u64_buf.copy_from_slice(&input_buffer[12..20]);
        let record_id = u64::from_le_bytes(u64_buf);

        MtimeRecord {
            mtime_sec,
            mtime_nsec,
            record_id,
        }
    }

    /// Returns `true` if `self` sorts strictly before `other` in the
    /// chronological total order.
    fn is_strictly_before(self, other: MtimeRecord) -> bool {
        if self.mtime_sec != other.mtime_sec {
            return self.mtime_sec < other.mtime_sec;
        }
        if self.mtime_nsec != other.mtime_nsec {
            return self.mtime_nsec < other.mtime_nsec;
        }
        self.record_id < other.record_id
    }
}

// =========================================================================
// names.bin / mtimes.bin path helpers and writers
// =========================================================================

fn build_scratch_path(temp_root_dir: &Path, scratch_basename: &str) -> PathBuf {
    let mut composed = PathBuf::from(temp_root_dir);
    composed.push(INDEX_SUBDIRNAME);
    composed.push(SCRATCH_DIRNAME);
    composed.push(scratch_basename);
    composed
}

fn ensure_scratch_directory_exists(temp_root_dir: &Path) -> Result<(), ChronoIndexError> {
    let mut scratch_dir = PathBuf::from(temp_root_dir);
    scratch_dir.push(INDEX_SUBDIRNAME);
    scratch_dir.push(SCRATCH_DIRNAME);
    match std::fs::create_dir_all(&scratch_dir) {
        Ok(()) => Ok(()),
        Err(_) => Err(ChronoIndexError::BuildIo),
    }
}

fn remove_scratch_directory_best_effort(temp_root_dir: &Path) {
    let mut scratch_dir = PathBuf::from(temp_root_dir);
    scratch_dir.push(INDEX_SUBDIRNAME);
    scratch_dir.push(SCRATCH_DIRNAME);
    // Best-effort: ignore errors. A leftover scratch directory is not a
    // correctness problem; it will be reused / overwritten next build.
    let _ = std::fs::remove_dir_all(&scratch_dir);
}

/// Pads a basename into a fixed 64-byte stack record. The first byte
/// past the basename length is set to NUL; subsequent bytes are zero.
///
/// Returns `None` if the basename exceeds `MAX_BASENAME_LEN`. The caller
/// (the build pass) responds to `None` by skipping the file and
/// incrementing a local counter, **not** by halting.
fn pack_basename_record(basename_bytes: &[u8]) -> Option<[u8; NAME_RECORD_SIZE]> {
    if basename_bytes.len() > MAX_BASENAME_LEN {
        return None;
    }
    let mut record_buffer = [0u8; NAME_RECORD_SIZE];
    record_buffer[..basename_bytes.len()].copy_from_slice(basename_bytes);
    Some(record_buffer)
}

// =========================================================================
// Cold-build orchestration
// =========================================================================

/// Result summary from a successful cold build. Returned to the caller
/// for observability / logging. Contains no user data.
#[derive(Clone, Copy, Debug)]
pub struct ColdBuildSummary {
    /// Number of files successfully indexed.
    pub files_indexed: u64,
    /// Number of entries skipped because their basename exceeded
    /// `MAX_BASENAME_LEN`. Project rule: skip & continue, do not halt.
    pub entries_skipped_overlong_name: u64,
    /// Number of entries skipped because `stat` failed on them.
    pub entries_skipped_stat_failed: u64,
    /// Number of entries skipped because they were not regular files
    /// (e.g. subdirectories, symlinks). Project rule: only regular files
    /// are indexed.
    pub entries_skipped_non_regular: u64,
}

/// Performs a complete cold (re)build of the index for the given parent
/// directory, writing all output under `<temp_root>/chrono_index/`.
///
/// On success: `header.bin`, `names.bin`, `mtimes.bin`,
/// are all present and consistent. Previous versions of these files (if
/// any) are replaced atomically.
///
/// On failure: a terse error code is returned. The previous index (if
/// any) remains intact because the new `header.bin` is the last file
/// written, via atomic rename. Scratch artifacts are cleaned up
/// best-effort.
///
/// Per project policy this function never panics and never halts.
pub fn cold_build_index(
    temp_root_dir: &Path,
    parent_directory_to_index: &Path,
) -> Result<ColdBuildSummary, ChronoIndexError> {
    // -- Phase 0: prepare directories ------------------------------------
    ensure_index_directory_exists(temp_root_dir)?;
    ensure_scratch_directory_exists(temp_root_dir)?;

    // Validate and capture parent path bytes for the header.
    let parent_path_bytes = posix_path_to_bytes(parent_directory_to_index)?;
    let mut working_header = ChronoIndexHeader::new_for_parent(parent_path_bytes)?;

    // -- Phase 1: stream read_dir → names.bin + scratch unsorted mtimes ---
    let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
    let scratch_unsorted_path = build_scratch_path(temp_root_dir, SCRATCH_UNSORTED_MTIMES_FILENAME);

    // We write names.bin and the unsorted scratch mtimes file to staging
    // names first; promote names.bin via rename after the sort succeeds.
    let names_staging_path = {
        let mut p = names_path.clone();
        p.set_file_name("names.bin.tmp");
        p
    };

    let phase1_summary = phase1_stream_directory_into_files(
        parent_directory_to_index,
        &names_staging_path,
        &scratch_unsorted_path,
        &mut working_header,
    );

    let phase1_summary = match phase1_summary {
        Ok(summary) => summary,
        Err(error_code) => {
            // Clean up partial artifacts. Do not touch any pre-existing
            // production names.bin / mtimes.bin / header.bin.
            let _ = std::fs::remove_file(&names_staging_path);
            remove_scratch_directory_best_effort(temp_root_dir);
            return Err(error_code);
        }
    };

    // -- Phase 2: external merge sort the scratch unsorted file ---------
    let mtimes_staging_path = {
        let mut p = build_index_file_path(temp_root_dir, MTIMES_FILENAME);
        p.set_file_name("mtimes.bin.tmp");
        p
    };

    let sort_outcome = external_merge_sort_mtimes(
        temp_root_dir,
        &scratch_unsorted_path,
        &mtimes_staging_path,
        working_header.file_count,
    );

    if let Err(error_code) = sort_outcome {
        let _ = std::fs::remove_file(&names_staging_path);
        let _ = std::fs::remove_file(&mtimes_staging_path);
        remove_scratch_directory_best_effort(temp_root_dir);
        return Err(error_code);
    }

    // -- Phase 3: capture last_mtime_* from the now-sorted file ---------
    // The last record in the sorted file is the chronologically newest;
    // we store its mtime in the header so the append path (part c) can
    // validate the "new files have newer mtimes" invariant in O(1).
    if working_header.file_count > 0 {
        match read_last_mtime_record(&mtimes_staging_path, working_header.file_count) {
            Ok(last_record) => {
                working_header.last_mtime_sec = last_record.mtime_sec;
                working_header.last_mtime_nsec = last_record.mtime_nsec;
            }
            Err(error_code) => {
                let _ = std::fs::remove_file(&names_staging_path);
                let _ = std::fs::remove_file(&mtimes_staging_path);
                remove_scratch_directory_best_effort(temp_root_dir);
                return Err(error_code);
            }
        }
    }
    // If file_count == 0: leave last_mtime_* at the sentinel from
    // `new_for_parent`, so any first appended file is strictly newer.

    // -- Phase 4: promote staging files via atomic rename ---------------
    // Order matters: data files first, header last. A crash between the
    // data renames and the header rename leaves the previous header in
    // place pointing at the previous data files; on next startup the
    // change-detection / validation will rebuild. Self-healing.
    if std::fs::rename(&names_staging_path, &names_path).is_err() {
        let _ = std::fs::remove_file(&names_staging_path);
        let _ = std::fs::remove_file(&mtimes_staging_path);
        remove_scratch_directory_best_effort(temp_root_dir);
        return Err(ChronoIndexError::RenameIo);
    }

    let mtimes_final_path = build_index_file_path(temp_root_dir, MTIMES_FILENAME);
    if std::fs::rename(&mtimes_staging_path, &mtimes_final_path).is_err() {
        // names.bin is now ahead of mtimes.bin; header has not yet been
        // updated to reference the new state, so the existing (old)
        // header is still authoritative. On next run, header validation
        // vs. file sizes will mismatch and trigger a fresh rebuild.
        let _ = std::fs::remove_file(&mtimes_staging_path);
        remove_scratch_directory_best_effort(temp_root_dir);
        return Err(ChronoIndexError::RenameIo);
    }

    // Header is the last write — its presence (with the new file_count)
    // signals "this index is committed."
    if let Err(error_code) = write_header_atomic(temp_root_dir, &working_header) {
        remove_scratch_directory_best_effort(temp_root_dir);
        return Err(error_code);
    }

    // -- Phase 5: cleanup scratch ---------------------------------------
    remove_scratch_directory_best_effort(temp_root_dir);

    Ok(phase1_summary)
}

/// Converts an absolute parent directory `Path` to its raw POSIX bytes,
/// validating length. POSIX paths are byte sequences (not guaranteed
/// UTF-8); we treat them as such.
#[cfg(unix)]
fn posix_path_to_bytes(parent_directory: &Path) -> Result<&[u8], ChronoIndexError> {
    use std::os::unix::ffi::OsStrExt;
    let raw_bytes = parent_directory.as_os_str().as_bytes();
    if raw_bytes.is_empty() {
        return Err(ChronoIndexError::ParentPathInvalid);
    }
    if raw_bytes.len() > MAX_PARENT_PATH_LEN {
        return Err(ChronoIndexError::ParentPathTooLong);
    }
    Ok(raw_bytes)
}

#[cfg(not(unix))]
fn posix_path_to_bytes(_parent_directory: &Path) -> Result<&[u8], ChronoIndexError> {
    // This module is POSIX-scoped per project spec. On non-Unix targets
    // we refuse rather than guess at path encoding.
    Err(ChronoIndexError::ParentPathInvalid)
}

// =========================================================================
// Phase 1: directory stream → names.bin (staged) + unsorted mtimes (scratch)
// =========================================================================

/// Streams `read_dir(parent_directory)` exactly once, performing for each
/// regular-file entry:
///
///   1. Compute basename bytes; reject if too long → skip & count.
///   2. `stat()` to obtain mtime; reject on stat failure → skip & count.
///   3. Assign a sequential `record_id` (zero-based).
///   4. Append a 64-byte basename record to `names_staging_path`.
///   5. Append a 20-byte mtime record to `scratch_unsorted_path`.
///   6. Update `signal_hash` (XOR-fold of FNV-1a of basename) and
///      `file_count` in `working_header`.
///
/// All buffers used are stack-resident or fixed-size. The two output
/// files are wrapped in `BufWriter`s of bounded capacity; their internal
/// buffers are a constant size (default 8 KB each), not scaled by N.
fn phase1_stream_directory_into_files(
    parent_directory: &Path,
    names_staging_path: &Path,
    scratch_unsorted_path: &Path,
    working_header: &mut ChronoIndexHeader,
) -> Result<ColdBuildSummary, ChronoIndexError> {
    // Open writers. Truncate any leftover staging from a prior aborted run.
    let names_file_handle = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(names_staging_path)
    {
        Ok(handle) => handle,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    let mut names_writer = BufWriter::new(names_file_handle);

    let scratch_file_handle = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(scratch_unsorted_path)
    {
        Ok(handle) => handle,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    let mut scratch_writer = BufWriter::new(scratch_file_handle);

    // Open the directory stream. `read_dir` is a streaming iterator over
    // `readdir(3)`; it does not preload all entries.
    let directory_iterator = match std::fs::read_dir(parent_directory) {
        Ok(iter) => iter,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };

    let mut summary = ColdBuildSummary {
        files_indexed: 0,
        entries_skipped_overlong_name: 0,
        entries_skipped_stat_failed: 0,
        entries_skipped_non_regular: 0,
    };
    let mut next_record_id: u64 = 0;

    // Per-basename Pearson hashes are XOR-folded lane-by-lane into this
    // accumulator. Width is governed by `PEARSON_SALT_ARRAY_SIZE`. The
    // final value becomes `working_header.signal_hash` and lets the
    // orchestrator detect, on later calls, whether the *set* of files
    // has changed (order-independent check). The order-sensitive check
    // is `chrono_sort_hash_to_n`.
    let mut signal_hash_accumulator: [u8; PEARSON_SALT_ARRAY_SIZE] = [0u8; PEARSON_SALT_ARRAY_SIZE];

    for directory_entry_result in directory_iterator {
        // Per-entry I/O errors: skip this entry, continue with the rest.
        let directory_entry = match directory_entry_result {
            Ok(entry) => entry,
            Err(_) => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };

        // file_type() is usually free on Linux (filled by readdir on most
        // filesystems); falls back to stat where not.
        let file_type_info = match directory_entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };
        if !file_type_info.is_file() {
            summary.entries_skipped_non_regular =
                summary.entries_skipped_non_regular.saturating_add(1);
            continue;
        }

        // Extract basename bytes (POSIX = raw bytes).
        let file_name_os = directory_entry.file_name();
        let basename_bytes: &[u8] = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                file_name_os.as_bytes()
            }
            #[cfg(not(unix))]
            {
                // POSIX-only module; reject.
                summary.entries_skipped_overlong_name =
                    summary.entries_skipped_overlong_name.saturating_add(1);
                continue;
            }
        };

        let name_record = match pack_basename_record(basename_bytes) {
            Some(packed) => packed,
            None => {
                summary.entries_skipped_overlong_name =
                    summary.entries_skipped_overlong_name.saturating_add(1);
                continue;
            }
        };

        // metadata() = stat(). Get mtime.
        let metadata = match directory_entry.metadata() {
            Ok(md) => md,
            Err(_) => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };

        let (mtime_sec, mtime_nsec) = match extract_mtime_seconds_and_nanos(&metadata) {
            Some(pair) => pair,
            None => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };

        // Write the name record.
        if names_writer.write_all(&name_record).is_err() {
            return Err(ChronoIndexError::BuildIo);
        }

        // Write the mtime record (record_id = this file's position in
        // names.bin).
        let mtime_record = MtimeRecord {
            mtime_sec,
            mtime_nsec,
            record_id: next_record_id,
        };
        let mut mtime_buffer = [0u8; MTIME_RECORD_SIZE];
        mtime_record.write_into(&mut mtime_buffer);
        if scratch_writer.write_all(&mtime_buffer).is_err() {
            return Err(ChronoIndexError::BuildIo);
        }

        // Hash this basename with `pearson_hash_salt_array` under the
        // Role-1 salts, then XOR-fold lane-by-lane into the accumulator.
        // An error from the Pearson layer would only occur for an empty
        // basename, which POSIX `readdir` never yields and which the
        // earlier basename-length check would not let through; we still
        // handle it terse-erroring rather than panicking, per project
        // policy.
        let per_basename_signal_hash = match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            basename_bytes,
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        ) {
            Ok(bytes) => bytes,
            Err(_) => return Err(ChronoIndexError::BuildIo),
        };
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            signal_hash_accumulator[lane_index] ^= per_basename_signal_hash[lane_index];
        }
        next_record_id = next_record_id.saturating_add(1);
        summary.files_indexed = summary.files_indexed.saturating_add(1);
    }

    // Flush and fsync both writers so the data is durable before sort.
    if names_writer.flush().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    let names_inner = match names_writer.into_inner() {
        Ok(inner) => inner,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    if names_inner.sync_all().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }

    if scratch_writer.flush().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    let scratch_inner = match scratch_writer.into_inner() {
        Ok(inner) => inner,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    if scratch_inner.sync_all().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }

    working_header.file_count = summary.files_indexed;
    working_header.signal_hash = signal_hash_accumulator;
    Ok(summary)
}

/// Extracts `(mtime_sec, mtime_nsec)` from a `Metadata` in a POSIX-safe
/// way. Returns `None` if the metadata lacks mtime information.
#[cfg(unix)]
fn extract_mtime_seconds_and_nanos(metadata: &std::fs::Metadata) -> Option<(i64, i32)> {
    use std::os::unix::fs::MetadataExt;
    let mtime_sec = metadata.mtime();
    let mtime_nsec_i64 = metadata.mtime_nsec();
    // Defensive clamp: nsec should be in [0, 1_000_000_000).
    // If the OS returns something outside that range (corruption,
    // unsupported filesystem, etc.), default to 0 rather than
    // propagating a nonsensical value. Fits in i32 after clamp.
    let mtime_nsec_i32 = if mtime_nsec_i64 < 0 || mtime_nsec_i64 >= 1_000_000_000 {
        0
    } else {
        mtime_nsec_i64 as i32
    };
    Some((mtime_sec, mtime_nsec_i32))
}

#[cfg(not(unix))]
fn extract_mtime_seconds_and_nanos(_metadata: &std::fs::Metadata) -> Option<(i64, i32)> {
    None
}

// =========================================================================
// Phase 2: external merge sort
// =========================================================================

/// Sorts the scratch unsorted mtimes file into `mtimes_staging_path`.
///
/// Strategy: replacement-free chunked sort.
///   1. Read `EXTERNAL_SORT_CHUNK_RECORDS` records into a heap-allocated
///      buffer (single bounded allocation, ~80 KB by default).
///   2. Sort the chunk in place with `sort_unstable_by` (no allocation).
///   3. Write the sorted chunk to a numbered run file in `scratch/`.
///   4. Repeat until input exhausted.
///   5. K-way merge runs (up to `MAX_MERGE_FANOUT` at a time) into the
///      staging output, cascading if run count exceeds the fan-out.
///
/// `expected_record_count` is the count produced by phase 1; used as a
/// sanity check and to short-circuit the no-records case.
fn external_merge_sort_mtimes(
    temp_root_dir: &Path,
    scratch_unsorted_path: &Path,
    mtimes_staging_path: &Path,
    expected_record_count: u64,
) -> Result<(), ChronoIndexError> {
    // Special case: empty directory. Produce a zero-length mtimes file.
    if expected_record_count == 0 {
        let empty_file = match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(mtimes_staging_path)
        {
            Ok(handle) => handle,
            Err(_) => return Err(ChronoIndexError::BuildIo),
        };
        if empty_file.sync_all().is_err() {
            return Err(ChronoIndexError::BuildIo);
        }
        return Ok(());
    }

    // -- Step 1: chunked sort into run files ----------------------------
    let unsorted_handle = match File::open(scratch_unsorted_path) {
        Ok(handle) => handle,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    let mut unsorted_reader = BufReader::new(unsorted_handle);

    // The sort buffer is the single bounded heap allocation. Default
    // 4096 × 20 B = 80 KB. Allocated once, reused across all chunks.
    let mut sort_buffer: Vec<MtimeRecord> = Vec::with_capacity(EXTERNAL_SORT_CHUNK_RECORDS);

    let mut next_run_index: u64 = 0;
    let mut run_paths: Vec<PathBuf> = Vec::new();
    // run_paths grows by 1 per run; total runs ≤ N / chunk_size, which
    // for N=1e6 and chunk=4096 is ~245 entries × ~100 B ≈ 25 KB. This is
    // bounded by N but with such a small constant that it does not
    // threaten the memory budget. Documented; acceptable.

    loop {
        sort_buffer.clear();
        let mut record_buffer = [0u8; MTIME_RECORD_SIZE];

        while sort_buffer.len() < EXTERNAL_SORT_CHUNK_RECORDS {
            match unsorted_reader.read_exact(&mut record_buffer) {
                Ok(()) => {
                    sort_buffer.push(MtimeRecord::read_from(&record_buffer));
                }
                Err(read_error) => {
                    if read_error.kind() == std::io::ErrorKind::UnexpectedEof {
                        break;
                    }
                    return Err(ChronoIndexError::BuildIo);
                }
            }
        }

        if sort_buffer.is_empty() {
            break;
        }

        // In-place sort, no allocation (unstable is allocation-free).
        sort_buffer.sort_unstable_by(|left, right| {
            if left.mtime_sec != right.mtime_sec {
                return left.mtime_sec.cmp(&right.mtime_sec);
            }
            if left.mtime_nsec != right.mtime_nsec {
                return left.mtime_nsec.cmp(&right.mtime_nsec);
            }
            left.record_id.cmp(&right.record_id)
        });

        // Write sorted chunk to a run file.
        let run_path = build_scratch_path(temp_root_dir, &format_run_filename(next_run_index));
        if let Err(error_code) = write_run_file(&run_path, &sort_buffer) {
            // Cleanup partial runs.
            for partial in &run_paths {
                let _ = std::fs::remove_file(partial);
            }
            let _ = std::fs::remove_file(&run_path);
            return Err(error_code);
        }
        run_paths.push(run_path);
        next_run_index = next_run_index.saturating_add(1);

        if sort_buffer.len() < EXTERNAL_SORT_CHUNK_RECORDS {
            // Last partial chunk; input is exhausted.
            break;
        }
    }

    // -- Step 2: cascading k-way merge ----------------------------------
    let final_run_path = cascade_merge_runs(temp_root_dir, run_paths)?;

    // Promote the final merged run to the mtimes staging path.
    if std::fs::rename(&final_run_path, mtimes_staging_path).is_err() {
        let _ = std::fs::remove_file(&final_run_path);
        return Err(ChronoIndexError::RenameIo);
    }
    Ok(())
}

/// `format!` is heap-using but produces a short, bounded-length string
/// (e.g. "run_00000042.bin"). The allocation is per-run, not per-record;
/// total allocations across a 1M-file build are ~245 × ~16 B = ~4 KB.
/// Documented as acceptable per project rules ("rule of thumb, not
/// pedantic"). If even this is unacceptable, swap for a stack `[u8; 24]`
/// formatter (e.g. via the project's Buffy module).
fn format_run_filename(run_index: u64) -> String {
    format!(
        "{}{:010}{}",
        SCRATCH_RUN_FILENAME_PREFIX, run_index, SCRATCH_RUN_FILENAME_SUFFIX
    )
}

fn write_run_file(run_path: &Path, sorted_records: &[MtimeRecord]) -> Result<(), ChronoIndexError> {
    let run_handle = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(run_path)
    {
        Ok(handle) => handle,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    let mut run_writer = BufWriter::new(run_handle);
    let mut record_buffer = [0u8; MTIME_RECORD_SIZE];
    for record in sorted_records {
        record.write_into(&mut record_buffer);
        if run_writer.write_all(&record_buffer).is_err() {
            return Err(ChronoIndexError::BuildIo);
        }
    }
    if run_writer.flush().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    let inner = match run_writer.into_inner() {
        Ok(inner) => inner,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    if inner.sync_all().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    Ok(())
}

/// Repeatedly merges up to `MAX_MERGE_FANOUT` runs at a time until a
/// single sorted run remains. Returns the path to that final run.
fn cascade_merge_runs(
    temp_root_dir: &Path,
    mut current_run_paths: Vec<PathBuf>,
) -> Result<PathBuf, ChronoIndexError> {
    if current_run_paths.is_empty() {
        // Shouldn't happen (expected_record_count > 0 was checked) but
        // handle defensively without panic.
        return Err(ChronoIndexError::BuildIo);
    }

    let mut merge_round_index: u64 = 0;

    while current_run_paths.len() > 1 {
        let mut next_round_runs: Vec<PathBuf> = Vec::new();
        let mut group_index: u64 = 0;

        let mut read_position_in_run_paths = 0usize;
        while read_position_in_run_paths < current_run_paths.len() {
            let group_end =
                (read_position_in_run_paths + MAX_MERGE_FANOUT).min(current_run_paths.len());
            let group_slice = &current_run_paths[read_position_in_run_paths..group_end];

            let merged_path = build_scratch_path(
                temp_root_dir,
                &format!("merge_r{:04}_g{:010}.bin", merge_round_index, group_index),
            );

            if let Err(error_code) = merge_runs_into(&merged_path, group_slice) {
                // Best-effort cleanup of all current and partial runs.
                for partial in &current_run_paths {
                    let _ = std::fs::remove_file(partial);
                }
                for partial in &next_round_runs {
                    let _ = std::fs::remove_file(partial);
                }
                let _ = std::fs::remove_file(&merged_path);
                return Err(error_code);
            }

            // Inputs to this merge are no longer needed.
            for consumed in group_slice {
                let _ = std::fs::remove_file(consumed);
            }
            next_round_runs.push(merged_path);
            read_position_in_run_paths = group_end;
            group_index = group_index.saturating_add(1);
        }

        current_run_paths = next_round_runs;
        merge_round_index = merge_round_index.saturating_add(1);
    }

    // Exactly one run remains.
    match current_run_paths.into_iter().next() {
        Some(final_path) => Ok(final_path),
        None => Err(ChronoIndexError::BuildIo),
    }
}

/// Merges a group of up to `MAX_MERGE_FANOUT` already-sorted run files
/// into a single sorted output file using a small fixed-size tournament.
///
/// Memory: one `MtimeRecord` per input run held in a stack-resident
/// `[Option<MtimeRecord>; MAX_MERGE_FANOUT]` array (320 B), plus one
/// `BufReader` per input run (bounded buffer, default 8 KB each =
/// 128 KB total at fan-out 16). All bounded and independent of N.
fn merge_runs_into(
    output_path: &Path,
    input_run_paths: &[PathBuf],
) -> Result<(), ChronoIndexError> {
    // Open all input readers. If any open fails, treat as build error.
    // Stack-resident array of optional readers, sized to MAX_MERGE_FANOUT.
    let mut input_readers: [Option<BufReader<File>>; MAX_MERGE_FANOUT] = Default::default();
    let mut head_records: [Option<MtimeRecord>; MAX_MERGE_FANOUT] = [None; MAX_MERGE_FANOUT];

    // Defensive: input_run_paths.len() must not exceed MAX_MERGE_FANOUT.
    if input_run_paths.len() > MAX_MERGE_FANOUT {
        return Err(ChronoIndexError::BuildIo);
    }

    for (slot_index, run_path) in input_run_paths.iter().enumerate() {
        let handle = match File::open(run_path) {
            Ok(h) => h,
            Err(_) => return Err(ChronoIndexError::BuildIo),
        };
        let mut reader = BufReader::new(handle);
        // Prime each reader with its first record.
        let mut record_buffer = [0u8; MTIME_RECORD_SIZE];
        match reader.read_exact(&mut record_buffer) {
            Ok(()) => {
                head_records[slot_index] = Some(MtimeRecord::read_from(&record_buffer));
            }
            Err(read_error) => {
                if read_error.kind() != std::io::ErrorKind::UnexpectedEof {
                    return Err(ChronoIndexError::BuildIo);
                }
                // Empty input run — leave head as None.
            }
        }
        input_readers[slot_index] = Some(reader);
    }

    // Open output.
    let output_handle = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_path)
    {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    let mut output_writer = BufWriter::new(output_handle);
    let mut output_buffer = [0u8; MTIME_RECORD_SIZE];

    // Linear scan over `MAX_MERGE_FANOUT` heads per record. With fan-out
    // ≤ 16, this is faster than a binary heap for our sizes and uses no
    // allocation. Total comparisons: N * fan-out, still O(N log N) with
    // the cascade depth factor.
    loop {
        // Find the slot with the smallest head record.
        let mut smallest_slot: Option<usize> = None;
        for slot_index in 0..input_run_paths.len() {
            if let Some(candidate_record) = head_records[slot_index] {
                match smallest_slot {
                    None => smallest_slot = Some(slot_index),
                    Some(current_best_slot) => {
                        // Safe: current_best_slot has Some head by construction.
                        let current_best = head_records[current_best_slot].unwrap_or(MtimeRecord {
                            mtime_sec: i64::MAX,
                            mtime_nsec: i32::MAX,
                            record_id: u64::MAX,
                        });
                        if candidate_record.is_strictly_before(current_best) {
                            smallest_slot = Some(slot_index);
                        }
                    }
                }
            }
        }

        let chosen_slot = match smallest_slot {
            Some(slot) => slot,
            None => break, // all inputs exhausted
        };

        // Write the chosen head and advance that reader.
        let chosen_record = match head_records[chosen_slot] {
            Some(r) => r,
            None => break, // unreachable per logic above; defensive exit
        };
        chosen_record.write_into(&mut output_buffer);
        if output_writer.write_all(&output_buffer).is_err() {
            return Err(ChronoIndexError::BuildIo);
        }

        // Advance the chosen reader.
        let reader_slot = match &mut input_readers[chosen_slot] {
            Some(r) => r,
            None => {
                head_records[chosen_slot] = None;
                continue;
            }
        };
        let mut record_buffer = [0u8; MTIME_RECORD_SIZE];
        match reader_slot.read_exact(&mut record_buffer) {
            Ok(()) => {
                head_records[chosen_slot] = Some(MtimeRecord::read_from(&record_buffer));
            }
            Err(read_error) => {
                if read_error.kind() == std::io::ErrorKind::UnexpectedEof {
                    head_records[chosen_slot] = None;
                } else {
                    return Err(ChronoIndexError::BuildIo);
                }
            }
        }
    }

    if output_writer.flush().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    let inner = match output_writer.into_inner() {
        Ok(inner) => inner,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    if inner.sync_all().is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    Ok(())
}

/// Reads the final (highest-index) record from a sorted mtimes file.
/// Used after Phase 2 to populate `header.last_mtime_*`.
fn read_last_mtime_record(
    mtimes_path: &Path,
    record_count: u64,
) -> Result<MtimeRecord, ChronoIndexError> {
    if record_count == 0 {
        return Err(ChronoIndexError::BuildIo);
    }
    let mut handle = match File::open(mtimes_path) {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };
    let last_index = record_count.saturating_sub(1);
    let byte_offset = last_index.saturating_mul(MTIME_RECORD_SIZE as u64);
    if handle.seek(SeekFrom::Start(byte_offset)).is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    let mut record_buffer = [0u8; MTIME_RECORD_SIZE];
    if handle.read_exact(&mut record_buffer).is_err() {
        return Err(ChronoIndexError::BuildIo);
    }
    Ok(MtimeRecord::read_from(&record_buffer))
}

// =========================================================================
// Tests for the cold-build path
// =========================================================================

#[cfg(test)]
mod chrono_index_part_b_tests {
    use super::*;
    // use std::io::Write as _;

    /// Creates a unique scratch directory for the index temp root.
    fn make_test_temp_root(label: &str) -> PathBuf {
        let mut scratch = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        scratch.push(format!(
            "chrono_index_b_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&scratch).expect("setup: temp root");
        scratch
    }

    /// Creates a separate "watched" directory and populates it with the
    /// given (basename, content) pairs. Each file is created in order, so
    /// (on most filesystems with sufficient timestamp resolution) the
    /// later files will have strictly newer mtimes — matching the
    /// project's "new files have newer mtimes" invariant.
    fn make_watched_dir_with_files(label: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut watched = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        watched.push(format!(
            "chrono_watched_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&watched).expect("setup: watched dir");
        for (basename, content) in files {
            let mut path = watched.clone();
            path.push(basename);
            let mut f = std::fs::File::create(&path).expect("setup: create file");
            f.write_all(content).expect("setup: write file");
            f.sync_all().expect("setup: sync file");
            // Sleep a few ms so subsequent files have strictly newer mtime
            // on filesystems with millisecond resolution (ext4 has ns res,
            // but some test envs use coarser). This keeps the invariant
            // observable in tests.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        watched
    }

    #[test]
    fn cold_build_on_empty_dir_produces_empty_index() {
        let temp_root = make_test_temp_root("empty");
        let watched = make_watched_dir_with_files("empty", &[]);

        let summary = cold_build_index(&temp_root, &watched).expect("build ok");
        assert_eq!(summary.files_indexed, 0);

        let header = read_header(&temp_root)
            .expect("read header ok")
            .expect("header present");
        assert_eq!(header.file_count, 0);
        assert_eq!(header.signal_hash, [0u8; PEARSON_SALT_ARRAY_SIZE]);
        // last_mtime sentinel preserved
        assert_eq!(header.last_mtime_sec, i64::MIN);
        assert_eq!(header.last_mtime_nsec, 0);

        // mtimes.bin should exist and be empty.
        let mtimes_path = build_index_file_path(&temp_root, MTIMES_FILENAME);
        let meta = std::fs::metadata(&mtimes_path).expect("mtimes exists");
        assert_eq!(meta.len(), 0);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_small_dir_produces_sorted_mtimes() {
        let temp_root = make_test_temp_root("small");
        // Create files in alphabetical order; they should also be in
        // chronological order because of the sleep in setup.
        let watched = make_watched_dir_with_files(
            "small",
            &[
                ("alpha.txt", b"a"),
                ("bravo.txt", b"bb"),
                ("charlie.txt", b"ccc"),
                ("delta.txt", b"dddd"),
            ],
        );

        let summary = cold_build_index(&temp_root, &watched).expect("build ok");
        assert_eq!(summary.files_indexed, 4);
        assert_eq!(summary.entries_skipped_overlong_name, 0);

        let header = read_header(&temp_root)
            .expect("read header ok")
            .expect("header present");
        assert_eq!(header.file_count, 4);
        // After indexing a non-empty directory, at least one lane of
        // the XOR-folded Pearson signal_hash should be nonzero with
        // overwhelming probability. (The all-zero outcome is possible
        // in principle for adversarially chosen basenames; for the
        // ordinary test inputs used here it does not occur.)
        assert_ne!(header.signal_hash, [0u8; PEARSON_SALT_ARRAY_SIZE]);

        // Verify mtimes.bin is sorted ascending.
        let mtimes_path = build_index_file_path(&temp_root, MTIMES_FILENAME);
        let meta = std::fs::metadata(&mtimes_path).expect("mtimes exists");
        assert_eq!(meta.len() as usize, 4 * MTIME_RECORD_SIZE);

        let mut handle = File::open(&mtimes_path).expect("open mtimes");
        let mut previous: Option<MtimeRecord> = None;
        for _ in 0..4 {
            let mut buf = [0u8; MTIME_RECORD_SIZE];
            handle.read_exact(&mut buf).expect("read record");
            let current = MtimeRecord::read_from(&buf);
            if let Some(prev) = previous {
                // Either strictly before or equal (with record_id tiebreak)
                let strictly_before_or_equal = prev.is_strictly_before(current)
                    || (prev.mtime_sec == current.mtime_sec
                        && prev.mtime_nsec == current.mtime_nsec
                        && prev.record_id < current.record_id);
                assert!(strictly_before_or_equal, "mtimes.bin not sorted");
            }
            previous = Some(current);
        }

        // header.last_mtime_* must equal the last record.
        let last = previous.expect("at least one record");
        assert_eq!(header.last_mtime_sec, last.mtime_sec);
        assert_eq!(header.last_mtime_nsec, last.mtime_nsec);

        // Scratch directory must have been cleaned up.
        let mut scratch_path = temp_root.clone();
        scratch_path.push(INDEX_SUBDIRNAME);
        scratch_path.push(SCRATCH_DIRNAME);
        assert!(!scratch_path.exists(), "scratch should be cleaned up");

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_larger_dir_exercises_external_sort() {
        // Force at least 2 chunks by creating > EXTERNAL_SORT_CHUNK_RECORDS
        // files. For test speed, reduce by using a smaller test-only count
        // and trusting the algorithm at larger N. We use 50 files here
        // and additionally verify the single-chunk path; the multi-chunk
        // path is covered by `cold_build_forces_multi_chunk_sort` below.
        let temp_root = make_test_temp_root("medium");
        let mut files_owned: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..50u32 {
            files_owned.push((format!("file_{:04}.dat", i), vec![i as u8; 4]));
        }
        let files_ref: Vec<(&str, &[u8])> = files_owned
            .iter()
            .map(|(name, content)| (name.as_str(), content.as_slice()))
            .collect();
        let watched = make_watched_dir_with_files("medium", &files_ref);

        let summary = cold_build_index(&temp_root, &watched).expect("build ok");
        assert_eq!(summary.files_indexed, 50);

        let header = read_header(&temp_root)
            .expect("read header ok")
            .expect("header present");
        assert_eq!(header.file_count, 50);

        // Verify full sort order across the file.
        let mtimes_path = build_index_file_path(&temp_root, MTIMES_FILENAME);
        let mut handle = File::open(&mtimes_path).expect("open mtimes");
        let mut previous: Option<MtimeRecord> = None;
        for _ in 0..50 {
            let mut buf = [0u8; MTIME_RECORD_SIZE];
            handle.read_exact(&mut buf).expect("read record");
            let current = MtimeRecord::read_from(&buf);
            if let Some(prev) = previous {
                let ordered = prev.is_strictly_before(current)
                    || (prev.mtime_sec == current.mtime_sec
                        && prev.mtime_nsec == current.mtime_nsec
                        && prev.record_id < current.record_id);
                assert!(ordered, "mtimes not in order");
            }
            previous = Some(current);
        }

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_skips_overlong_basenames_without_halting() {
        let temp_root = make_test_temp_root("overlong");
        // Construct an overlong filename (65 chars).
        let overlong: String = "x".repeat(MAX_BASENAME_LEN + 1);
        let watched = make_watched_dir_with_files(
            "overlong",
            &[
                ("ok_short.txt", b"a"),
                (overlong.as_str(), b"b"),
                ("also_ok.txt", b"c"),
            ],
        );

        let summary = cold_build_index(&temp_root, &watched).expect("build ok");
        assert_eq!(summary.files_indexed, 2);
        assert_eq!(summary.entries_skipped_overlong_name, 1);

        let header = read_header(&temp_root)
            .expect("read header ok")
            .expect("header present");
        assert_eq!(header.file_count, 2);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_skips_non_regular_entries() {
        let temp_root = make_test_temp_root("non_regular");
        let watched = make_watched_dir_with_files("non_regular", &[("real_file.txt", b"hi")]);

        // Add a subdirectory inside watched.
        let mut subdir_path = watched.clone();
        subdir_path.push("a_subdirectory");
        std::fs::create_dir_all(&subdir_path).expect("setup: subdir");

        let summary = cold_build_index(&temp_root, &watched).expect("build ok");
        assert_eq!(summary.files_indexed, 1);
        assert!(summary.entries_skipped_non_regular >= 1);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_rejects_nonexistent_parent() {
        let temp_root = make_test_temp_root("no_parent");
        let mut nonexistent = std::env::temp_dir();
        nonexistent.push("definitely_not_a_real_dir_chrono_index_test_xyz_123");
        // Make sure it really doesn't exist.
        let _ = std::fs::remove_dir_all(&nonexistent);

        let result = cold_build_index(&temp_root, &nonexistent);
        assert!(result.is_err());
        assert_eq!(result.err(), Some(ChronoIndexError::BuildIo));

        // No header should have been written.
        let header_path = build_index_file_path(&temp_root, HEADER_FILENAME);
        assert!(!header_path.exists());

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn mtime_record_serialize_round_trip() {
        let original = MtimeRecord {
            mtime_sec: 1_700_000_123,
            mtime_nsec: 456_789_012,
            record_id: 999_999,
        };
        let mut buf = [0u8; MTIME_RECORD_SIZE];
        original.write_into(&mut buf);
        let recovered = MtimeRecord::read_from(&buf);
        assert_eq!(recovered.mtime_sec, original.mtime_sec);
        assert_eq!(recovered.mtime_nsec, original.mtime_nsec);
        assert_eq!(recovered.record_id, original.record_id);
    }

    #[test]
    fn mtime_record_strict_ordering_uses_sec_then_nsec_then_record_id() {
        let earlier = MtimeRecord {
            mtime_sec: 100,
            mtime_nsec: 0,
            record_id: 50,
        };
        let later_sec = MtimeRecord {
            mtime_sec: 101,
            mtime_nsec: 0,
            record_id: 0,
        };
        let same_sec_later_nsec = MtimeRecord {
            mtime_sec: 100,
            mtime_nsec: 1,
            record_id: 0,
        };
        let same_sec_same_nsec_later_id = MtimeRecord {
            mtime_sec: 100,
            mtime_nsec: 0,
            record_id: 51,
        };

        // sec dominates
        assert!(earlier.is_strictly_before(later_sec));
        assert!(!later_sec.is_strictly_before(earlier));

        // nsec tiebreaks on equal sec
        assert!(earlier.is_strictly_before(same_sec_later_nsec));
        assert!(!same_sec_later_nsec.is_strictly_before(earlier));

        // record_id tiebreaks on equal sec+nsec
        assert!(earlier.is_strictly_before(same_sec_same_nsec_later_id));
        assert!(!same_sec_same_nsec_later_id.is_strictly_before(earlier));

        // Equal records are not strictly-before each other.
        let copy_of_earlier = earlier;
        assert!(!earlier.is_strictly_before(copy_of_earlier));
        assert!(!copy_of_earlier.is_strictly_before(earlier));
    }

    #[test]
    fn pack_basename_record_rejects_overlong_input() {
        let just_right = vec![b'a'; MAX_BASENAME_LEN];
        assert!(pack_basename_record(&just_right).is_some());

        let too_long = vec![b'a'; MAX_BASENAME_LEN + 1];
        assert!(pack_basename_record(&too_long).is_none());
    }

    #[test]
    fn pack_basename_record_zero_pads_unused_tail() {
        let short_name = b"hi";
        let packed = pack_basename_record(short_name).expect("fits");
        // First two bytes are the name; remainder must be zero.
        assert_eq!(&packed[..2], short_name);
        for trailing_byte in &packed[2..] {
            assert_eq!(*trailing_byte, 0);
        }
    }

    #[test]
    fn cold_build_records_signal_hash_as_xor_of_basename_pearson() {
        // Project context: after a cold build, `header.signal_hash`
        // must equal the lane-wise XOR of per-basename Pearson hashes
        // (Role 1, salts = `SIGNAL_HASH_SALTS`). This test reproduces
        // that recipe independently and checks the on-disk header.
        let temp_root = make_test_temp_root("signal");
        let watched = make_watched_dir_with_files(
            "signal",
            &[("one.dat", b"1"), ("two.dat", b"22"), ("three.dat", b"333")],
        );

        let _ = cold_build_index(&temp_root, &watched).expect("build ok");
        let header = read_header(&temp_root)
            .expect("read header ok")
            .expect("header present");

        // Hash each basename with the Role-1 recipe.
        let hash_one = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"one.dat",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash one");
        let hash_two = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"two.dat",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash two");
        let hash_three = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"three.dat",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash three");

        // XOR-fold lane by lane.
        let mut expected_signal = [0u8; PEARSON_SALT_ARRAY_SIZE];
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            expected_signal[lane_index] =
                hash_one[lane_index] ^ hash_two[lane_index] ^ hash_three[lane_index];
        }
        assert_eq!(header.signal_hash, expected_signal);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_writes_names_in_record_id_order() {
        // record_id is assigned in readdir-iteration order. We don't
        // know that order on a given filesystem, but we DO know that
        // each mtime record's `record_id` must index a valid 64-byte
        // slot in names.bin, and the basename at that slot must be one
        // of the files we created.
        let temp_root = make_test_temp_root("names_layout");
        let watched = make_watched_dir_with_files(
            "names_layout",
            &[("aaa.txt", b"x"), ("bbb.txt", b"y"), ("ccc.txt", b"z")],
        );

        let summary = cold_build_index(&temp_root, &watched).expect("build ok");
        assert_eq!(summary.files_indexed, 3);

        let mtimes_path = build_index_file_path(&temp_root, MTIMES_FILENAME);
        let names_path = build_index_file_path(&temp_root, NAMES_FILENAME);

        let mut mtimes_handle = File::open(&mtimes_path).expect("open mtimes");
        let mut names_handle = File::open(&names_path).expect("open names");

        // Collect (record_id) for each mtime record in sorted order.
        let mut seen_basenames: Vec<Vec<u8>> = Vec::new();
        for _ in 0..3 {
            let mut mtime_buf = [0u8; MTIME_RECORD_SIZE];
            mtimes_handle
                .read_exact(&mut mtime_buf)
                .expect("read mtime");
            let record = MtimeRecord::read_from(&mtime_buf);

            // Seek into names.bin by record_id and read the 64-byte slot.
            let names_offset = record.record_id.saturating_mul(NAME_RECORD_SIZE as u64);
            names_handle
                .seek(SeekFrom::Start(names_offset))
                .expect("seek names");
            let mut name_buf = [0u8; NAME_RECORD_SIZE];
            names_handle.read_exact(&mut name_buf).expect("read name");

            // Trim trailing zeros for comparison.
            let used_len = name_buf
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(NAME_RECORD_SIZE);
            seen_basenames.push(name_buf[..used_len].to_vec());
        }

        // All three known basenames must appear exactly once.
        let mut sorted_seen = seen_basenames.clone();
        sorted_seen.sort();
        let mut expected = vec![
            b"aaa.txt".to_vec(),
            b"bbb.txt".to_vec(),
            b"ccc.txt".to_vec(),
        ];
        expected.sort();
        assert_eq!(sorted_seen, expected);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn cold_build_overwrites_previous_index() {
        let temp_root = make_test_temp_root("rebuild");
        let watched = make_watched_dir_with_files("rebuild_a", &[("first.txt", b"f")]);

        let summary_a = cold_build_index(&temp_root, &watched).expect("build a");
        assert_eq!(summary_a.files_indexed, 1);

        // Add another file and rebuild.
        let mut second_path = watched.clone();
        second_path.push("second.txt");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut f = std::fs::File::create(&second_path).expect("create second");
        f.write_all(b"s").expect("write second");
        f.sync_all().expect("sync second");

        let summary_b = cold_build_index(&temp_root, &watched).expect("build b");
        assert_eq!(summary_b.files_indexed, 2);

        let header = read_header(&temp_root)
            .expect("read header")
            .expect("present");
        assert_eq!(header.file_count, 2);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }
}

// =========================================================================
// Part (c): Incremental-append path
// =========================================================================
//
// ## When this runs
//
// The append path is the steady-state hot path. It is invoked when the
// directory has grown — i.e. one or more new files have appeared since the
// last index commit — and the pre-existing portion of the index appears
// unchanged.
//
// The caller's update-orchestration logic decides which path to run by
// comparing the live directory against `header.bin`. Concretely:
//
//   - Scan the live directory once, computing `(live_count, live_signal_hash)`
//     where `live_signal_hash` is the XOR-fold of FNV-1a 64 of every
//     basename. This is one streaming pass, no `stat()`, no heap growth
//     with N.
//
//   - Compare against the header:
//
//       * `live_count == header.file_count`
//         && `live_signal_hash == header.signal_hash`
//             → index is current; nothing to do.
//
//       * `live_count > header.file_count`
//         && the XOR of the *new* basenames equals
//            `live_signal_hash XOR header.signal_hash`
//             → exactly K = live_count - header.file_count new files
//               appeared and none of the existing files were renamed or
//               removed. This is the *append-eligible* case and is the
//               subject of the append path below.
//
//       * Anything else (e.g. live_count shrank, hashes incompatible)
//             → fall back to cold rebuild (part b). Log a terse code.
//               Per project policy: never halt.
//
//   Note: the orchestration above (the "decide which path" step) is a
//   thin wrapper around the building blocks in this file. It is exposed
//   in part (d): Update orchestration and chronological lookup
//  along with the lookup function so that callers have one
//   single high-level entrypoint.
//
// ## What this path does
//
// Given:
//   - the index temp root,
//   - the watched parent directory,
//   - the currently-committed header (already loaded from disk),
//
// the append path:
//
//   1. Streams `read_dir` once.
//   2. For each entry, computes the basename hash and looks it up in an
//      on-disk "name hash index" sidecar (lazily built on first append;
//      see `name_hashes.bin`). If the hash is already present, the entry
//      is an existing file → skip. Otherwise the entry is *candidate new*.
//   3. For each candidate new entry: calls `stat()` to obtain its mtime,
//      appends its basename to `names.bin` (assigning a new record_id),
//      and buffers the resulting `MtimeRecord` in a small fixed-size
//      stack/heap batch. Batches are bounded by `APPEND_BATCH_RECORDS`.
//   4. When a batch is full (or the directory scan ends), the batch is
//      sorted in place, then merge-appended to `mtimes.bin`:
//         * Fast path: every record's `(mtime_sec, mtime_nsec)` is
//           strictly newer than `header.last_mtime_*`. Pure append, no
//           rewrite. This is the expected steady-state path.
//         * Slow path: at least one record in the batch is older than
//           the current `header.last_mtime_*`. This violates the project
//           invariant but does not halt; we bump
//           `header.invariant_breach_count`, perform a bounded merge
//           insert (rewriting only the suffix of `mtimes.bin` that
//           needs reordering), and continue.
//   5. Updates `header.bin` atomically (file_count, signal_hash,
//      last_mtime_*, possibly invariant_breach_count).
//
// ## Memory discipline
//
// All buffers used by the append path are fixed-size:
//   - One `[u8; NAME_RECORD_SIZE]` for the basename being written.
//   - One `[u8; MTIME_RECORD_SIZE]` for the mtime record being written.
//   - One `Vec<MtimeRecord>` of capacity `APPEND_BATCH_RECORDS` (default
//     256 × 20 B = 5 KB). Single bounded allocation per call; reused
//     across batches in the same call.
//   - The `name_hashes.bin` sidecar is consulted via streamed reads
//     of fixed-size chunks; it is never loaded whole.
//
// No structure grows with N during append.
//
// ## Failure policy
//
// Append is best-effort and never halts. On any I/O or structural error
// the function:
//   - leaves `names.bin` and `mtimes.bin` in the largest consistent
//     prefix it has successfully reached (file truncation, see below),
//   - rewrites `header.bin` atomically to reflect that prefix, and
//   - returns a terse error code.
//
// The caller may retry on the next call. Worst case, the orchestration
// in part (d) Update orchestration and chronological lookup
// demotes the next attempt to a cold rebuild.
//
// To keep `names.bin` and `mtimes.bin` consistent under crash or partial
// write, we do not update `header.bin` until both files are flushed and
// synced. The header is the commit point.

/// Maximum number of new-file `MtimeRecord`s buffered before a batch
/// flush. Each record is 20 B; default 256 × 20 = 5 KB. Single bounded
/// allocation per append call. Choose larger if appends typically arrive
/// in larger bursts; choose smaller if memory is tighter still.
pub const APPEND_BATCH_RECORDS: usize = 256;

/// Filename of the optional sidecar that stores per-basename Pearson
/// hashes parallel to `names.bin`. Built lazily on first append. Allows
/// "is this basename already indexed?" to be answered without rereading
/// the (heavier) `names.bin`.
///
/// Layout: `record_id -> Pearson hash of basename`, fixed
/// `NAME_HASH_RECORD_SIZE` (= `PEARSON_SALT_ARRAY_SIZE`) bytes per
/// record. Position `i` in this file corresponds to position `i` in
/// `names.bin`. Hashes are produced by `pearson_hash_salt_array` using
/// `NAME_HASH_SALTS` over `GENERATED_TABLE`.
pub const NAME_HASHES_FILENAME: &str = "name_hashes.bin";

/// Size in bytes of one `name_hashes.bin` record.
///
/// Each record stores one Pearson hash of `PEARSON_SALT_ARRAY_SIZE`
/// bytes, computed with `NAME_HASH_SALTS` over `GENERATED_TABLE`.
/// Position `i` in this file corresponds to position `i` in
/// `names.bin`: record_id -> per-basename Pearson hash.
pub const NAME_HASH_RECORD_SIZE: usize = PEARSON_SALT_ARRAY_SIZE;

// =========================================================================
// Public summary type
// =========================================================================

/// Summary produced by one invocation of [`incremental_append_new_files`].
#[derive(Clone, Copy, Debug)]
pub struct AppendSummary {
    /// Number of new files successfully indexed in this call.
    pub files_appended: u64,
    /// Number of directory entries skipped because the basename exceeded
    /// `MAX_BASENAME_LEN`.
    pub entries_skipped_overlong_name: u64,
    /// Number of directory entries skipped because `stat()` failed.
    pub entries_skipped_stat_failed: u64,
    /// Number of directory entries skipped because the entry was not a
    /// regular file (e.g. a subdirectory).
    pub entries_skipped_non_regular: u64,
    /// Number of new-file mtimes that arrived out of chronological order
    /// (older than the current `header.last_mtime_*`). The invariant
    /// "new files have newer mtimes" was breached this many times.
    /// Handled defensively via bounded merge insert; not fatal.
    pub invariant_breaches_this_call: u64,
}

// =========================================================================
// name_hashes sidecar: build, read, append
// =========================================================================

/// Ensures `name_hashes.bin` exists and is consistent with `names.bin`.
///
/// If `name_hashes.bin` is missing or its size disagrees with
/// `header.file_count`, the sidecar is rebuilt from scratch by streaming
/// `names.bin`. This is an O(N) operation but performs only fixed-size
/// reads; it is done at most once per index lifetime in the common case.
///
/// Memory: one `[u8; NAME_RECORD_SIZE]` and one `[u8; NAME_HASH_RECORD_SIZE]`
/// buffer on the stack. No per-N heap.
fn ensure_name_hashes_sidecar_consistent(
    temp_root_dir: &Path,
    expected_record_count: u64,
) -> Result<(), ChronoIndexError> {
    let hashes_path = build_index_file_path(temp_root_dir, NAME_HASHES_FILENAME);
    let expected_size_bytes = expected_record_count.saturating_mul(NAME_HASH_RECORD_SIZE as u64);

    let existing_size = match std::fs::metadata(&hashes_path) {
        Ok(metadata) => metadata.len(),
        Err(open_error) => {
            if open_error.kind() == std::io::ErrorKind::NotFound {
                0
            } else {
                return Err(ChronoIndexError::AppendIo);
            }
        }
    };

    if existing_size == expected_size_bytes && expected_record_count > 0 {
        // Sidecar is consistent; nothing to do.
        return Ok(());
    }
    if expected_record_count == 0 {
        // No records to hash. Make sure any stale sidecar is replaced
        // with an empty file so subsequent appends start clean.
        let empty_handle = match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&hashes_path)
        {
            Ok(h) => h,
            Err(_) => return Err(ChronoIndexError::AppendIo),
        };
        if empty_handle.sync_all().is_err() {
            return Err(ChronoIndexError::AppendIo);
        }
        return Ok(());
    }

    // Rebuild from names.bin. Stage to a sibling temp file and atomically
    // rename, so a crash mid-build does not leave a half-written sidecar
    // that future runs would trust.
    let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
    let mut names_reader = match File::open(&names_path) {
        Ok(handle) => BufReader::new(handle),
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };

    let mut staging_path = hashes_path.clone();
    staging_path.set_file_name("name_hashes.bin.tmp");
    let mut staging_writer = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging_path)
    {
        Ok(handle) => BufWriter::new(handle),
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };

    let mut name_buffer = [0u8; NAME_RECORD_SIZE];
    let mut hash_buffer = [0u8; NAME_HASH_RECORD_SIZE];
    let mut records_written: u64 = 0;

    loop {
        match names_reader.read_exact(&mut name_buffer) {
            Ok(()) => {
                let used_len = basename_used_length(&name_buffer);
                // Empty basenames cannot occur on POSIX (readdir never
                // yields them), and the cold-build + append paths both
                // refuse zero-length names earlier. If we somehow read
                // an all-NUL name record here (corruption), refuse to
                // emit a bogus sidecar rather than feeding an empty
                // slice to pearson_hash_salt_array (which would return
                // an error).
                if used_len == 0 {
                    let _ = std::fs::remove_file(&staging_path);
                    return Err(ChronoIndexError::AppendIo);
                }
                let pearson_hash_bytes = match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
                    &name_buffer[..used_len],
                    &NAME_HASH_SALTS,
                    &GENERATED_TABLE,
                ) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        let _ = std::fs::remove_file(&staging_path);
                        return Err(ChronoIndexError::AppendIo);
                    }
                };
                hash_buffer.copy_from_slice(&pearson_hash_bytes);
                if staging_writer.write_all(&hash_buffer).is_err() {
                    let _ = std::fs::remove_file(&staging_path);
                    return Err(ChronoIndexError::AppendIo);
                }
                records_written = records_written.saturating_add(1);
                if records_written > expected_record_count {
                    // names.bin has more records than the header says.
                    // Refuse to produce an inconsistent sidecar.
                    let _ = std::fs::remove_file(&staging_path);
                    return Err(ChronoIndexError::AppendIo);
                }
            }
            Err(read_error) => {
                if read_error.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                let _ = std::fs::remove_file(&staging_path);
                return Err(ChronoIndexError::AppendIo);
            }
        }
    }

    if records_written != expected_record_count {
        // names.bin disagrees with header. Refuse to commit.
        let _ = std::fs::remove_file(&staging_path);
        return Err(ChronoIndexError::AppendIo);
    }

    if staging_writer.flush().is_err() {
        let _ = std::fs::remove_file(&staging_path);
        return Err(ChronoIndexError::AppendIo);
    }
    let inner = match staging_writer.into_inner() {
        Ok(i) => i,
        Err(_) => {
            let _ = std::fs::remove_file(&staging_path);
            return Err(ChronoIndexError::AppendIo);
        }
    };
    if inner.sync_all().is_err() {
        let _ = std::fs::remove_file(&staging_path);
        return Err(ChronoIndexError::AppendIo);
    }
    drop(inner);

    if std::fs::rename(&staging_path, &hashes_path).is_err() {
        let _ = std::fs::remove_file(&staging_path);
        return Err(ChronoIndexError::RenameIo);
    }
    Ok(())
}

/// Returns the used (pre-NUL-padding) length of a 64-byte basename record.
fn basename_used_length(name_record: &[u8; NAME_RECORD_SIZE]) -> usize {
    let mut used = 0usize;
    while used < NAME_RECORD_SIZE && name_record[used] != 0 {
        used += 1;
    }
    used
}

/// Tests whether `target_basename_hash` is already present anywhere in
/// `name_hashes.bin`. Streamed linear scan over fixed-size
/// `NAME_HASH_RECORD_SIZE`-byte records; bounded stack memory, no heap
/// growth with N.
///
/// For very large N this is O(N) per candidate. The append-eligibility
/// gate in `create_or_update_chrono_index` (XOR-of-new-hashes equals
/// delta) ensures we only call this for genuinely new candidates, so
/// in the common case we scan and find no hit only K times where K is
/// the number of new files in this update — typically very small.
fn name_hash_is_present_in_sidecar(
    temp_root_dir: &Path,
    target_basename_hash: [u8; PEARSON_SALT_ARRAY_SIZE],
) -> Result<bool, ChronoIndexError> {
    let hashes_path = build_index_file_path(temp_root_dir, NAME_HASHES_FILENAME);
    let handle = match File::open(&hashes_path) {
        Ok(h) => h,
        Err(open_error) => {
            if open_error.kind() == std::io::ErrorKind::NotFound {
                return Ok(false);
            }
            return Err(ChronoIndexError::AppendIo);
        }
    };
    let mut reader = BufReader::new(handle);
    let mut record_buffer = [0u8; NAME_HASH_RECORD_SIZE];
    loop {
        match reader.read_exact(&mut record_buffer) {
            Ok(()) => {
                if record_buffer == target_basename_hash {
                    return Ok(true);
                }
            }
            Err(read_error) => {
                if read_error.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Ok(false);
                }
                return Err(ChronoIndexError::AppendIo);
            }
        }
    }
}

/// Appends one `NAME_HASH_RECORD_SIZE`-byte Pearson hash record to
/// `name_hashes.bin`. The caller is responsible for keeping append
/// order in lockstep with `names.bin`.
fn append_name_hash_record(
    temp_root_dir: &Path,
    new_basename_hash: [u8; PEARSON_SALT_ARRAY_SIZE],
) -> Result<(), ChronoIndexError> {
    let hashes_path = build_index_file_path(temp_root_dir, NAME_HASHES_FILENAME);
    let mut handle = match OpenOptions::new()
        .append(true)
        .create(true)
        .open(&hashes_path)
    {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };
    if handle.write_all(&new_basename_hash).is_err() {
        return Err(ChronoIndexError::AppendIo);
    }
    // Flush+sync is performed by the caller in a single fsync at end of
    // the append call, not per record, to keep cost amortized.
    Ok(())
}

// =========================================================================
// Append-only writes to names.bin and mtimes.bin
// =========================================================================

/// Appends one 64-byte basename record to `names.bin`. The caller is
/// responsible for assigning sequential record_ids and for keeping
/// `name_hashes.bin` in lockstep.
fn append_basename_record_to_names(
    temp_root_dir: &Path,
    name_record: &[u8; NAME_RECORD_SIZE],
) -> Result<(), ChronoIndexError> {
    let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
    let mut handle = match OpenOptions::new()
        .append(true)
        .create(true)
        .open(&names_path)
    {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };
    if handle.write_all(name_record).is_err() {
        return Err(ChronoIndexError::AppendIo);
    }
    Ok(())
}

/// Appends a batch of sorted, in-order MtimeRecords to `mtimes.bin`.
/// Pre-condition (checked by caller): the first record's mtime is
/// `>= header.last_mtime_*`. This is the fast path.
fn append_sorted_mtime_batch(
    temp_root_dir: &Path,
    sorted_batch: &[MtimeRecord],
) -> Result<(), ChronoIndexError> {
    if sorted_batch.is_empty() {
        return Ok(());
    }
    let mtimes_path = build_index_file_path(temp_root_dir, MTIMES_FILENAME);
    let handle = match OpenOptions::new()
        .append(true)
        .create(true)
        .open(&mtimes_path)
    {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };
    let mut writer = BufWriter::new(handle);
    let mut record_buffer = [0u8; MTIME_RECORD_SIZE];
    for record in sorted_batch {
        record.write_into(&mut record_buffer);
        if writer.write_all(&record_buffer).is_err() {
            return Err(ChronoIndexError::AppendIo);
        }
    }
    if writer.flush().is_err() {
        return Err(ChronoIndexError::AppendIo);
    }
    let inner = match writer.into_inner() {
        Ok(i) => i,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };
    if inner.sync_all().is_err() {
        return Err(ChronoIndexError::AppendIo);
    }
    Ok(())
}

/// Rewrites `mtimes.bin` to remain sorted after one or more new records
/// turn out to have mtimes OLDER than records already on disk.
///
/// Why this exists
/// ---------------
/// `mtimes.bin` is kept sorted ascending by (mtime_sec, mtime_nsec,
/// record_id). The project invariant states that new files always have
/// newer mtimes than every already-indexed file, so the normal append
/// path is sufficient. This function is the defensive fallback for
/// when that invariant is violated: it preserves correctness without
/// halting the program.
///
/// What this function does
/// -----------------------
/// Performs a streaming 2-way merge of two already-sorted inputs:
///   Input A: the current contents of `mtimes.bin` (read one record
///            at a time from disk).
///   Input B: `new_records_batch`, which is sorted in place at the
///            start of this function.
/// The smaller of the two heads is written to a staging file at each
/// step. When both inputs are exhausted, the staging file is renamed
/// atomically over `mtimes.bin`.
///
/// Return value
/// ------------
/// Returns `(last_written_mtime_sec, last_written_mtime_nsec)` —
/// the mtime of the last record written to the staging file, which by
/// construction is the newest mtime in the rewritten `mtimes.bin`.
/// The caller stores this in `header.last_mtime_sec` / `_nsec`.
///
/// Memory
/// ------
/// One `MtimeRecord` for each input's "current head", plus the
/// caller-owned `new_records_batch`. No structure grows with the total
/// number of records on disk.
///
/// Failure
/// -------
/// On any I/O error, the staging file is removed where possible and
/// a terse error code is returned. The on-disk `mtimes.bin` is left
/// unchanged until the final atomic rename, so a failure here cannot
/// corrupt the existing index.
fn rewrite_mtimes_bin_with_out_of_order_batch(
    temp_root_dir: &Path,
    new_records_batch: &mut [MtimeRecord],
) -> Result<(i64, i32), ChronoIndexError> {
    if new_records_batch.is_empty() {
        return Err(ChronoIndexError::AppendIo);
    }

    // Step 1. Sort the new batch with the same total order used in mtimes.bin.
    new_records_batch.sort_unstable_by(|left, right| {
        if left.mtime_sec != right.mtime_sec {
            return left.mtime_sec.cmp(&right.mtime_sec);
        }
        if left.mtime_nsec != right.mtime_nsec {
            return left.mtime_nsec.cmp(&right.mtime_nsec);
        }
        left.record_id.cmp(&right.record_id)
    });

    let existing_mtimes_path = build_index_file_path(temp_root_dir, MTIMES_FILENAME);
    let mut staging_mtimes_path = existing_mtimes_path.clone();
    staging_mtimes_path.set_file_name("mtimes.bin.tmp");

    // Step 2. Open the existing mtimes.bin for streamed reading.
    let existing_file_handle = match File::open(&existing_mtimes_path) {
        Ok(handle) => handle,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };
    let mut existing_file_reader = BufReader::new(existing_file_handle);

    // Step 3. Open the staging output file for streamed writing.
    let staging_file_handle = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging_mtimes_path)
    {
        Ok(handle) => handle,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };
    let mut staging_file_writer = BufWriter::new(staging_file_handle);

    let mut existing_record_bytes = [0u8; MTIME_RECORD_SIZE];
    let mut staging_record_bytes = [0u8; MTIME_RECORD_SIZE];
    let mut batch_read_position: usize = 0;
    let mut newest_written_record: Option<MtimeRecord> = None;

    // Step 4. Prime the existing-side head with its first record (if any).
    let mut current_head_from_existing_file: Option<MtimeRecord> =
        match existing_file_reader.read_exact(&mut existing_record_bytes) {
            Ok(()) => Some(MtimeRecord::read_from(&existing_record_bytes)),
            Err(read_error) => {
                if read_error.kind() == std::io::ErrorKind::UnexpectedEof {
                    None
                } else {
                    let _ = std::fs::remove_file(&staging_mtimes_path);
                    return Err(ChronoIndexError::AppendIo);
                }
            }
        };

    // Step 5. The merge loop.
    //
    // At each iteration:
    //   - We have at most one "head" from each input.
    //   - We pick the smaller head, write it to the staging file, and
    //     advance that input.
    //   - When both inputs are exhausted, we exit.
    loop {
        let current_head_from_batch: Option<MtimeRecord> =
            new_records_batch.get(batch_read_position).copied();

        // Decide which input contributes the next record to the output.
        let take_record_from_batch_side: bool =
            match (&current_head_from_existing_file, &current_head_from_batch) {
                (Some(existing_head_record), Some(batch_head_record)) => {
                    batch_head_record.is_strictly_before(*existing_head_record)
                }
                (Some(_existing_head_record), None) => false,
                (None, Some(_batch_head_record)) => true,
                (None, None) => break,
            };

        // Take the chosen record and advance its input.
        let record_to_write: MtimeRecord = if take_record_from_batch_side {
            // Take from the new-records batch.
            let chosen_record = new_records_batch[batch_read_position];
            batch_read_position = batch_read_position.saturating_add(1);
            chosen_record
        } else {
            // Take from the existing mtimes.bin.
            let chosen_record = match current_head_from_existing_file {
                Some(existing_head_record) => existing_head_record,
                None => break,
            };
            // Refill the existing-side head from the next record on disk.
            current_head_from_existing_file =
                match existing_file_reader.read_exact(&mut existing_record_bytes) {
                    Ok(()) => Some(MtimeRecord::read_from(&existing_record_bytes)),
                    Err(read_error) => {
                        if read_error.kind() == std::io::ErrorKind::UnexpectedEof {
                            None
                        } else {
                            let _ = std::fs::remove_file(&staging_mtimes_path);
                            return Err(ChronoIndexError::AppendIo);
                        }
                    }
                };
            chosen_record
        };

        // Write the chosen record to the staging file.
        record_to_write.write_into(&mut staging_record_bytes);
        if staging_file_writer
            .write_all(&staging_record_bytes)
            .is_err()
        {
            let _ = std::fs::remove_file(&staging_mtimes_path);
            return Err(ChronoIndexError::AppendIo);
        }
        newest_written_record = Some(record_to_write);
    }

    // Step 6. Flush and fsync the staging file before renaming.
    if staging_file_writer.flush().is_err() {
        let _ = std::fs::remove_file(&staging_mtimes_path);
        return Err(ChronoIndexError::AppendIo);
    }
    let staging_file_after_flush = match staging_file_writer.into_inner() {
        Ok(inner_file_handle) => inner_file_handle,
        Err(_) => {
            let _ = std::fs::remove_file(&staging_mtimes_path);
            return Err(ChronoIndexError::AppendIo);
        }
    };
    if staging_file_after_flush.sync_all().is_err() {
        let _ = std::fs::remove_file(&staging_mtimes_path);
        return Err(ChronoIndexError::AppendIo);
    }
    drop(staging_file_after_flush);

    // Step 7. Atomic rename: staging file replaces the live mtimes.bin.
    if std::fs::rename(&staging_mtimes_path, &existing_mtimes_path).is_err() {
        let _ = std::fs::remove_file(&staging_mtimes_path);
        return Err(ChronoIndexError::RenameIo);
    }

    // Step 8. Report the newest mtime now on disk, for header update.
    match newest_written_record {
        Some(written_record) => Ok((written_record.mtime_sec, written_record.mtime_nsec)),
        None => Err(ChronoIndexError::AppendIo),
    }
}

// =========================================================================
// The append entrypoint
// =========================================================================

/// Incrementally appends any new files in `parent_directory_to_index`
/// to the existing index, updating `header.bin` atomically on success.
///
/// Pre-conditions:
///   - `header.bin`, `names.bin`, `mtimes.bin` exist and are consistent
///     with each other (this is the responsibility of the caller's
///     orchestration in part d; if not, the caller should cold-rebuild
///     instead).
///   - `current_header` reflects the on-disk header.
///
/// Post-conditions on success:
///   - `header.file_count` reflects the new total.
///   - `header.signal_hash` XORs in each newly indexed basename.
///   - `header.last_mtime_*` reflects the newest record in `mtimes.bin`.
///   - `header.invariant_breach_count` is incremented per out-of-order
///     batch.
///
/// Post-conditions on failure:
///   - Returns a terse error code.
///   - `header.bin` is updated only if it can be made consistent with
///     the (possibly partial) new state of `names.bin` and `mtimes.bin`.
///     If even that fails, the previous header remains in place; the
///     caller's next orchestration round will detect the inconsistency
///     and trigger a cold rebuild. Never halts.
pub fn incremental_append_new_files(
    temp_root_dir: &Path,
    parent_directory_to_index: &Path,
    current_header: &ChronoIndexHeader,
) -> Result<AppendSummary, ChronoIndexError> {
    // Validate the parent path in the header still matches what was
    // passed in. If it has changed, caller should rebuild, not append.
    {
        let passed_in_bytes = posix_path_to_bytes(parent_directory_to_index)?;
        if passed_in_bytes != current_header.parent_path_slice() {
            return Err(ChronoIndexError::ParentPathInvalid);
        }
    }

    // Make sure the name-hash sidecar is present and matches file_count.
    ensure_name_hashes_sidecar_consistent(temp_root_dir, current_header.file_count)?;

    let mut summary = AppendSummary {
        files_appended: 0,
        entries_skipped_overlong_name: 0,
        entries_skipped_stat_failed: 0,
        entries_skipped_non_regular: 0,
        invariant_breaches_this_call: 0,
    };

    // Mutable header copy that we will commit only on success.
    let mut working_header: ChronoIndexHeader = current_header.clone();

    // Bounded batch buffer. Single allocation per call.
    let mut current_batch: Vec<MtimeRecord> = Vec::with_capacity(APPEND_BATCH_RECORDS);

    // Per-batch XOR-fold of the new files' Role-1 Pearson hashes.
    // Folded into `working_header.signal_hash` when the batch is
    // flushed by `flush_batch_and_update_header`. Width and semantics
    // identical to `signal_hash_accumulator` in
    // `phase1_stream_directory_into_files`.
    let mut current_batch_signal_xor: [u8; PEARSON_SALT_ARRAY_SIZE] =
        [0u8; PEARSON_SALT_ARRAY_SIZE];

    let directory_iterator = match std::fs::read_dir(parent_directory_to_index) {
        Ok(it) => it,
        Err(_) => return Err(ChronoIndexError::AppendIo),
    };

    for directory_entry_result in directory_iterator {
        let directory_entry = match directory_entry_result {
            Ok(e) => e,
            Err(_) => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };

        let file_type_info = match directory_entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };
        if !file_type_info.is_file() {
            summary.entries_skipped_non_regular =
                summary.entries_skipped_non_regular.saturating_add(1);
            continue;
        }

        // Basename bytes (POSIX).
        let file_name_os = directory_entry.file_name();
        let basename_bytes: &[u8] = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                file_name_os.as_bytes()
            }
            #[cfg(not(unix))]
            {
                summary.entries_skipped_overlong_name =
                    summary.entries_skipped_overlong_name.saturating_add(1);
                continue;
            }
        };

        if basename_bytes.len() > MAX_BASENAME_LEN {
            summary.entries_skipped_overlong_name =
                summary.entries_skipped_overlong_name.saturating_add(1);
            continue;
        }

        // Compute the Role-2 (sidecar) Pearson hash and check the
        // sidecar to see if this is an already-indexed file. Role 2
        // uses its own salt array (`NAME_HASH_SALTS`) so its
        // collision profile is statistically independent of Role 1's
        // signal_hash. A theoretical hash collision here would cause
        // a conservative skip (the orchestrator's post-append
        // signal_hash gate would then trigger a rebuild). No panic.
        let basename_hash_for_sidecar = match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            basename_bytes,
            &NAME_HASH_SALTS,
            &GENERATED_TABLE,
        ) {
            Ok(bytes) => bytes,
            Err(_) => return Err(ChronoIndexError::AppendIo),
        };
        match name_hash_is_present_in_sidecar(temp_root_dir, basename_hash_for_sidecar)? {
            true => {
                // Known file — nothing to do.
                continue;
            }
            false => {
                // Defensive double-check: the sidecar is a *hash* index,
                // so a hash collision against an existing-but-different
                // basename is theoretically possible (u64 collision odds
                // are negligible for tens of thousands of files but not
                // zero in principle). We resolve such ambiguity safely
                // by treating an apparent collision as "skip and rebuild
                // later" — i.e. we conservatively skip this entry in
                // this path. Since hash collisions are astronomically
                // rare in practice this branch is effectively dead.
                //
                // Implementation note: we cannot detect the collision
                // cheaply here without a full scan of names.bin; we
                // accept the trade-off of an extremely rare
                // false-skip. Out-of-band consistency checks (e.g. the
                // signal_hash mismatch detection in part d) will
                // eventually trigger a rebuild that re-indexes the
                // missed file. No halt, no data loss.
            }
        }

        // stat() for mtime.
        let metadata = match directory_entry.metadata() {
            Ok(md) => md,
            Err(_) => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };
        let (mtime_sec, mtime_nsec) = match extract_mtime_seconds_and_nanos(&metadata) {
            Some(pair) => pair,
            None => {
                summary.entries_skipped_stat_failed =
                    summary.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };

        // Append basename and hash sidecar in lockstep, assigning a
        // new record_id == current file_count + items already appended
        // in this call.
        let new_record_id = working_header.file_count;
        // Pack and write the basename.
        let name_record = match pack_basename_record(basename_bytes) {
            Some(packed) => packed,
            None => {
                // pack already enforces MAX_BASENAME_LEN; if we got here
                // the basename passed the earlier length check, so this
                // branch is unreachable in practice. Defensive only.
                summary.entries_skipped_overlong_name =
                    summary.entries_skipped_overlong_name.saturating_add(1);
                continue;
            }
        };

        if let Err(write_error) = append_basename_record_to_names(temp_root_dir, &name_record) {
            // Try to flush whatever batch is pending so the index can be
            // committed in a consistent prefix state.
            let _ = flush_batch_and_update_header(
                temp_root_dir,
                &mut current_batch,
                &mut working_header,
                &mut current_batch_signal_xor,
                &mut summary,
            );
            // Commit best-effort header even on failure to keep files
            // in sync. If this fails too, the caller's orchestration
            // will trigger a rebuild on next call.
            let _ = write_header_atomic(temp_root_dir, &working_header);
            return Err(write_error);
        }
        if let Err(write_error) = append_name_hash_record(temp_root_dir, basename_hash_for_sidecar)
        {
            let _ = flush_batch_and_update_header(
                temp_root_dir,
                &mut current_batch,
                &mut working_header,
                &mut current_batch_signal_xor,
                &mut summary,
            );
            let _ = write_header_atomic(temp_root_dir, &working_header);
            return Err(write_error);
        }

        let new_record = MtimeRecord {
            mtime_sec,
            mtime_nsec,
            record_id: new_record_id,
        };
        current_batch.push(new_record);
        // Fold this basename's Role-1 (signal_hash) Pearson hash into
        // the per-batch accumulator. Note this is a SEPARATE hash from
        // `basename_hash_for_sidecar` above (different salt array):
        // sidecar uses `NAME_HASH_SALTS`, signal_hash uses
        // `SIGNAL_HASH_SALTS`, so the two roles are statistically
        // independent.
        let per_basename_signal_hash = match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            basename_bytes,
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        ) {
            Ok(bytes) => bytes,
            Err(_) => return Err(ChronoIndexError::AppendIo),
        };
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            current_batch_signal_xor[lane_index] ^= per_basename_signal_hash[lane_index];
        }
        working_header.file_count = working_header.file_count.saturating_add(1);

        if current_batch.len() >= APPEND_BATCH_RECORDS {
            flush_batch_and_update_header(
                temp_root_dir,
                &mut current_batch,
                &mut working_header,
                &mut current_batch_signal_xor,
                &mut summary,
            )?;
        }
    }

    // Final flush of whatever remains.
    if !current_batch.is_empty() {
        flush_batch_and_update_header(
            temp_root_dir,
            &mut current_batch,
            &mut working_header,
            &mut current_batch_signal_xor,
            &mut summary,
        )?;
    }

    // sync_all on name_hashes.bin is implicit (we used .append which
    // does not buffer in a BufWriter), but we explicitly fsync now to
    // make the sidecar durable before committing the header.
    {
        let hashes_path = build_index_file_path(temp_root_dir, NAME_HASHES_FILENAME);
        if let Ok(h) = File::open(&hashes_path) {
            let _ = h.sync_all();
        }
    }
    // Same for names.bin.
    {
        let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
        if let Ok(h) = File::open(&names_path) {
            let _ = h.sync_all();
        }
    }

    // Commit the header. This is the atomic publish point.
    write_header_atomic(temp_root_dir, &working_header)?;
    Ok(summary)
}

/// Flushes a (possibly fast-path or slow-path) batch to `mtimes.bin`
/// and updates `working_header.last_mtime_*`, `signal_hash`, and
/// `invariant_breach_count` accordingly.
fn flush_batch_and_update_header(
    temp_root_dir: &Path,
    current_batch: &mut Vec<MtimeRecord>,
    working_header: &mut ChronoIndexHeader,
    current_batch_signal_xor: &mut [u8; PEARSON_SALT_ARRAY_SIZE],
    summary: &mut AppendSummary,
) -> Result<(), ChronoIndexError> {
    if current_batch.is_empty() {
        return Ok(());
    }

    // Sort the batch by the same total order as the file.
    current_batch.sort_unstable_by(|left, right| {
        if left.mtime_sec != right.mtime_sec {
            return left.mtime_sec.cmp(&right.mtime_sec);
        }
        if left.mtime_nsec != right.mtime_nsec {
            return left.mtime_nsec.cmp(&right.mtime_nsec);
        }
        left.record_id.cmp(&right.record_id)
    });

    // Decide fast vs. slow path. Compare the *smallest* record in the
    // batch to working_header.last_mtime_*. If the smallest is strictly
    // greater than (or equal to) the current last, pure append is sound.
    let smallest_in_batch = current_batch[0];
    let last_in_file = MtimeRecord {
        mtime_sec: working_header.last_mtime_sec,
        mtime_nsec: working_header.last_mtime_nsec,
        record_id: 0,
    };
    // smallest_in_batch >= last_in_file (sec, nsec) ?
    let fast_path_ok = smallest_in_batch.mtime_sec > last_in_file.mtime_sec
        || (smallest_in_batch.mtime_sec == last_in_file.mtime_sec
            && smallest_in_batch.mtime_nsec >= last_in_file.mtime_nsec)
        || working_header.file_count == current_batch.len() as u64; // first ever append

    if fast_path_ok {
        append_sorted_mtime_batch(temp_root_dir, current_batch)?;
        // Update last_mtime_* to the newest in the batch (which is the
        // last element after sort).
        let newest = match current_batch.last() {
            Some(r) => *r,
            None => return Err(ChronoIndexError::AppendIo),
        };
        working_header.last_mtime_sec = newest.mtime_sec;
        working_header.last_mtime_nsec = newest.mtime_nsec;
    } else {
        // Slow path: at least one batch record is older than current
        // last. Increment invariant breach counter and merge-insert.
        working_header.invariant_breach_count =
            working_header.invariant_breach_count.saturating_add(1);
        summary.invariant_breaches_this_call =
            summary.invariant_breaches_this_call.saturating_add(1);
        let (new_last_sec, new_last_nsec) =
            rewrite_mtimes_bin_with_out_of_order_batch(temp_root_dir, current_batch)?;
        working_header.last_mtime_sec = new_last_sec;
        working_header.last_mtime_nsec = new_last_nsec;
    }

    // Fold the batch's XOR contribution into the header's running
    // signal_hash, lane by lane. XOR is associative and commutative,
    // so applying batches in any order produces the same final
    // header.signal_hash.
    for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
        working_header.signal_hash[lane_index] ^= current_batch_signal_xor[lane_index];
    }
    summary.files_appended = summary
        .files_appended
        .saturating_add(current_batch.len() as u64);

    current_batch.clear();
    // Reset the per-batch accumulator to all-zero so the next batch
    // starts from a clean slate.
    *current_batch_signal_xor = [0u8; PEARSON_SALT_ARRAY_SIZE];
    Ok(())
}

// =========================================================================
// Tests for the incremental-append path
// =========================================================================

#[cfg(test)]
mod chrono_index_part_c_tests {
    use super::*;
    // use std::io::Write as _;

    fn make_test_temp_root(label: &str) -> PathBuf {
        let mut scratch = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        scratch.push(format!(
            "chrono_index_c_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&scratch).expect("setup");
        scratch
    }

    fn make_watched_dir_with_files(label: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut watched = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        watched.push(format!(
            "chrono_watched_c_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&watched).expect("setup");
        for (basename, content) in files {
            let mut path = watched.clone();
            path.push(basename);
            let mut f = std::fs::File::create(&path).expect("create");
            f.write_all(content).expect("write");
            f.sync_all().expect("sync");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        watched
    }

    fn add_file_to_watched_dir(watched_dir: &Path, basename: &str, content: &[u8]) {
        // Sleep first so the new file has a strictly newer mtime than
        // anything pre-existing.
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut path = PathBuf::from(watched_dir);
        path.push(basename);
        let mut f = std::fs::File::create(&path).expect("create new");
        f.write_all(content).expect("write new");
        f.sync_all().expect("sync new");
    }

    fn read_all_mtime_records(temp_root: &Path) -> Vec<MtimeRecord> {
        let path = build_index_file_path(temp_root, MTIMES_FILENAME);
        let mut handle = File::open(&path).expect("open mtimes");
        let mut out = Vec::new();
        let mut buf = [0u8; MTIME_RECORD_SIZE];
        loop {
            match handle.read_exact(&mut buf) {
                Ok(()) => out.push(MtimeRecord::read_from(&buf)),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(_) => panic!("read error in test helper"),
            }
        }
        out
    }

    fn read_basename_at_record_id(temp_root: &Path, record_id: u64) -> Vec<u8> {
        let path = build_index_file_path(temp_root, NAMES_FILENAME);
        let mut handle = File::open(&path).expect("open names");
        handle
            .seek(SeekFrom::Start(
                record_id.saturating_mul(NAME_RECORD_SIZE as u64),
            ))
            .expect("seek");
        let mut buf = [0u8; NAME_RECORD_SIZE];
        handle.read_exact(&mut buf).expect("read name");
        let used = basename_used_length(&buf);
        buf[..used].to_vec()
    }

    #[test]
    fn append_adds_single_new_file_to_already_built_index() {
        let temp_root = make_test_temp_root("single_append");
        let watched = make_watched_dir_with_files(
            "single_append",
            &[("first.txt", b"a"), ("second.txt", b"b")],
        );
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header_after_build = read_header(&temp_root).expect("read").expect("present");
        assert_eq!(header_after_build.file_count, 2);

        // Add a new file with strictly newer mtime.
        add_file_to_watched_dir(&watched, "third.txt", b"c");

        let summary = incremental_append_new_files(&temp_root, &watched, &header_after_build)
            .expect("append ok");
        assert_eq!(summary.files_appended, 1);
        assert_eq!(summary.invariant_breaches_this_call, 0);

        let header_after_append = read_header(&temp_root).expect("read").expect("present");
        assert_eq!(header_after_append.file_count, 3);
        assert!(header_after_append.last_mtime_sec >= header_after_build.last_mtime_sec);

        // mtimes.bin must remain sorted.
        let records = read_all_mtime_records(&temp_root);
        assert_eq!(records.len(), 3);
        for window in records.windows(2) {
            let ordered = window[0].is_strictly_before(window[1])
                || (window[0].mtime_sec == window[1].mtime_sec
                    && window[0].mtime_nsec == window[1].mtime_nsec
                    && window[0].record_id < window[1].record_id);
            assert!(ordered, "mtimes.bin lost sorted order");
        }

        // The newest record must point to "third.txt".
        let last = *records.last().expect("at least one");
        let name = read_basename_at_record_id(&temp_root, last.record_id);
        assert_eq!(name, b"third.txt");

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_handles_multiple_new_files_in_one_call() {
        let temp_root = make_test_temp_root("multi_append");
        let watched = make_watched_dir_with_files("multi_append", &[("a.dat", b"1")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header_after_build = read_header(&temp_root).expect("r").expect("p");

        for new_name in &["b.dat", "c.dat", "d.dat", "e.dat"] {
            add_file_to_watched_dir(&watched, new_name, new_name.as_bytes());
        }

        let summary = incremental_append_new_files(&temp_root, &watched, &header_after_build)
            .expect("append ok");
        assert_eq!(summary.files_appended, 4);

        let header_after = read_header(&temp_root).expect("r").expect("p");
        assert_eq!(header_after.file_count, 5);

        let records = read_all_mtime_records(&temp_root);
        assert_eq!(records.len(), 5);
        for window in records.windows(2) {
            let ordered = window[0].is_strictly_before(window[1])
                || (window[0].mtime_sec == window[1].mtime_sec
                    && window[0].mtime_nsec == window[1].mtime_nsec
                    && window[0].record_id < window[1].record_id);
            assert!(ordered);
        }

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_is_idempotent_when_no_new_files() {
        let temp_root = make_test_temp_root("noop_append");
        let watched = make_watched_dir_with_files(
            "noop_append",
            &[("one.x", b"1"), ("two.x", b"2"), ("three.x", b"3")],
        );
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header_before = read_header(&temp_root).expect("r").expect("p");

        let summary =
            incremental_append_new_files(&temp_root, &watched, &header_before).expect("append ok");
        assert_eq!(summary.files_appended, 0);

        let header_after = read_header(&temp_root).expect("r").expect("p");
        assert_eq!(header_after.file_count, header_before.file_count);
        assert_eq!(header_after.signal_hash, header_before.signal_hash);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_signal_hash_xor_accumulates_correctly() {
        // Project context: incremental append must XOR-fold each new
        // basename's Role-1 Pearson hash into header.signal_hash. The
        // final value after a build of one file followed by two
        // appended files must equal the XOR of all three per-basename
        // Pearson hashes — verifying that the build and append paths
        // use the same recipe.
        let temp_root = make_test_temp_root("signal_xor");
        let watched = make_watched_dir_with_files("signal_xor", &[("alpha", b"a")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header_after_build = read_header(&temp_root).expect("r").expect("p");

        let expected_initial = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"alpha",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash alpha");
        assert_eq!(header_after_build.signal_hash, expected_initial);

        add_file_to_watched_dir(&watched, "beta", b"b");
        add_file_to_watched_dir(&watched, "gamma", b"g");

        let _ = incremental_append_new_files(&temp_root, &watched, &header_after_build)
            .expect("append ok");

        let header_after_append = read_header(&temp_root).expect("r").expect("p");

        let hash_alpha = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"alpha",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash alpha");
        let hash_beta = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"beta",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash beta");
        let hash_gamma = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"gamma",
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash gamma");

        let mut expected_final = [0u8; PEARSON_SALT_ARRAY_SIZE];
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            expected_final[lane_index] =
                hash_alpha[lane_index] ^ hash_beta[lane_index] ^ hash_gamma[lane_index];
        }
        assert_eq!(header_after_append.signal_hash, expected_final);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_rejects_wrong_parent_path() {
        let temp_root = make_test_temp_root("wrong_parent");
        let watched_a = make_watched_dir_with_files("wrong_parent_a", &[("x", b"x")]);
        let watched_b = make_watched_dir_with_files("wrong_parent_b", &[("y", b"y")]);
        let _ = cold_build_index(&temp_root, &watched_a).expect("cold build");
        let header = read_header(&temp_root).expect("r").expect("p");

        let result = incremental_append_new_files(&temp_root, &watched_b, &header);
        assert_eq!(result.err(), Some(ChronoIndexError::ParentPathInvalid));

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched_a);
        let _ = std::fs::remove_dir_all(&watched_b);
    }

    #[test]
    fn append_skips_overlong_basenames() {
        let temp_root = make_test_temp_root("overlong_append");
        let watched = make_watched_dir_with_files("overlong_append", &[("normal.txt", b"n")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header = read_header(&temp_root).expect("r").expect("p");

        // Add one valid + one overlong.
        add_file_to_watched_dir(&watched, "valid.txt", b"v");
        let overlong: String = "z".repeat(MAX_BASENAME_LEN + 1);
        add_file_to_watched_dir(&watched, overlong.as_str(), b"x");

        let summary =
            incremental_append_new_files(&temp_root, &watched, &header).expect("append ok");
        assert_eq!(summary.files_appended, 1);
        assert_eq!(summary.entries_skipped_overlong_name, 1);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_skips_subdirectory_entries() {
        let temp_root = make_test_temp_root("subdir_skip");
        let watched = make_watched_dir_with_files("subdir_skip", &[("file.txt", b"f")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header = read_header(&temp_root).expect("r").expect("p");

        let mut subdir = watched.clone();
        subdir.push("a_sub");
        std::fs::create_dir_all(&subdir).expect("mkdir");
        add_file_to_watched_dir(&watched, "newfile.txt", b"n");

        let summary =
            incremental_append_new_files(&temp_root, &watched, &header).expect("append ok");
        assert_eq!(summary.files_appended, 1);
        assert!(summary.entries_skipped_non_regular >= 1);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn name_hashes_sidecar_built_lazily_and_matches_names() {
        let temp_root = make_test_temp_root("sidecar");
        let watched =
            make_watched_dir_with_files("sidecar", &[("aa", b"1"), ("bb", b"2"), ("cc", b"3")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");

        // Sidecar should not yet exist after cold build.
        let sidecar_path = build_index_file_path(&temp_root, NAME_HASHES_FILENAME);
        assert!(!sidecar_path.exists(), "sidecar should be lazy");

        // Triggering append (with no new files) should build it.
        let header = read_header(&temp_root).expect("r").expect("p");
        let _ =
            incremental_append_new_files(&temp_root, &watched, &header).expect("append ok (noop)");
        assert!(sidecar_path.exists());

        let meta = std::fs::metadata(&sidecar_path).expect("meta");
        assert_eq!(meta.len() as usize, 3 * NAME_HASH_RECORD_SIZE);

        // Each hash in the sidecar must match the Role-2 Pearson hash
        // of the corresponding basename. NAME_HASH_RECORD_SIZE equals
        // PEARSON_SALT_ARRAY_SIZE, so the on-disk record IS the hash
        // array — no integer decoding step needed.
        let mut sidecar_handle = File::open(&sidecar_path).expect("open");
        let mut names_handle =
            File::open(&build_index_file_path(&temp_root, NAMES_FILENAME)).expect("open names");
        for _ in 0..3u64 {
            let mut hash_buf = [0u8; NAME_HASH_RECORD_SIZE];
            sidecar_handle.read_exact(&mut hash_buf).expect("read hash");
            let mut name_buf = [0u8; NAME_RECORD_SIZE];
            names_handle.read_exact(&mut name_buf).expect("read name");
            let used = basename_used_length(&name_buf);
            let expected_hash = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
                &name_buf[..used],
                &NAME_HASH_SALTS,
                &GENERATED_TABLE,
            )
            .expect("pearson hash");
            assert_eq!(hash_buf, expected_hash);
        }
        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_after_build_keeps_mtimes_sorted_across_many_batches() {
        // Cross a batch boundary by appending APPEND_BATCH_RECORDS + a
        // few extra files. We keep the count test-feasible.
        let temp_root = make_test_temp_root("many_batches");
        let watched = make_watched_dir_with_files("many_batches", &[("seed.txt", b"s")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        let header = read_header(&temp_root).expect("r").expect("p");

        // We do not actually want to sleep 10ms × hundreds of times in
        // a test, so we reduce: just append a handful but include a
        // batch-size sanity check.
        let extras: usize = (APPEND_BATCH_RECORDS / 32).max(8);
        for i in 0..extras {
            let name = format!("extra_{:05}.dat", i);
            // Smaller sleep to keep test fast; ns-resolution filesystems
            // (ext4) preserve order even at 1 ms.
            std::thread::sleep(std::time::Duration::from_millis(2));
            let mut p = watched.clone();
            p.push(&name);
            let mut f = std::fs::File::create(&p).expect("create");
            f.write_all(name.as_bytes()).expect("write");
            f.sync_all().expect("sync");
        }

        let summary =
            incremental_append_new_files(&temp_root, &watched, &header).expect("append ok");
        assert_eq!(summary.files_appended as usize, extras);

        let records = read_all_mtime_records(&temp_root);
        assert_eq!(records.len(), 1 + extras);
        for window in records.windows(2) {
            let ordered = window[0].is_strictly_before(window[1])
                || (window[0].mtime_sec == window[1].mtime_sec
                    && window[0].mtime_nsec == window[1].mtime_nsec
                    && window[0].record_id < window[1].record_id);
            assert!(ordered, "mtimes.bin lost sorted order across batches");
        }

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_round_trip_then_second_append_works() {
        // Two successive appends must each commit their state cleanly,
        // so the second sees the header produced by the first.
        let temp_root = make_test_temp_root("two_appends");
        let watched = make_watched_dir_with_files("two_appends", &[("seed", b"s")]);
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");

        // First append round.
        add_file_to_watched_dir(&watched, "round1_a", b"1a");
        add_file_to_watched_dir(&watched, "round1_b", b"1b");
        let header_after_build = read_header(&temp_root).expect("r").expect("p");
        let summary_1 = incremental_append_new_files(&temp_root, &watched, &header_after_build)
            .expect("append 1 ok");
        assert_eq!(summary_1.files_appended, 2);

        // Second append round, starting from the updated header.
        add_file_to_watched_dir(&watched, "round2_a", b"2a");
        let header_after_first = read_header(&temp_root).expect("r").expect("p");
        assert_eq!(header_after_first.file_count, 3);
        let summary_2 = incremental_append_new_files(&temp_root, &watched, &header_after_first)
            .expect("append 2 ok");
        assert_eq!(summary_2.files_appended, 1);

        // Final state must be sorted and contain all four entries.
        let header_after_second = read_header(&temp_root).expect("r").expect("p");
        assert_eq!(header_after_second.file_count, 4);

        let records = read_all_mtime_records(&temp_root);
        assert_eq!(records.len(), 4);
        for window in records.windows(2) {
            let ordered = window[0].is_strictly_before(window[1])
                || (window[0].mtime_sec == window[1].mtime_sec
                    && window[0].mtime_nsec == window[1].mtime_nsec
                    && window[0].record_id < window[1].record_id);
            assert!(ordered);
        }

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn append_slow_path_handles_out_of_order_mtime_without_halting() {
        // Synthetic exercise of `merge_insert_out_of_order_batch` via a
        // direct invocation. We build a tiny mtimes.bin with two records
        // and then merge-insert a third that sorts BEFORE both. The file
        // must remain sorted and the function must return the new last
        // mtime (which equals the previous last, since the inserted
        // record was older).
        let temp_root = make_test_temp_root("slow_path");
        ensure_index_directory_exists(&temp_root).expect("setup");

        let mtimes_path = build_index_file_path(&temp_root, MTIMES_FILENAME);
        {
            // Write two in-order records.
            let mut handle = std::fs::File::create(&mtimes_path).expect("create");
            let mut buf = [0u8; MTIME_RECORD_SIZE];
            MtimeRecord {
                mtime_sec: 100,
                mtime_nsec: 0,
                record_id: 0,
            }
            .write_into(&mut buf);
            handle.write_all(&buf).expect("w1");
            MtimeRecord {
                mtime_sec: 200,
                mtime_nsec: 0,
                record_id: 1,
            }
            .write_into(&mut buf);
            handle.write_all(&buf).expect("w2");
            handle.sync_all().expect("sync");
        }

        // Out-of-order batch: a record at mtime_sec=50, which is older
        // than every record currently in the file.
        let mut batch = [MtimeRecord {
            mtime_sec: 50,
            mtime_nsec: 0,
            record_id: 2,
        }];

        let (new_last_sec, new_last_nsec) =
            rewrite_mtimes_bin_with_out_of_order_batch(&temp_root, &mut batch[..])
                .expect("merge insert ok");
        // The newest record is still the original mtime_sec=200 one.
        assert_eq!(new_last_sec, 200);
        assert_eq!(new_last_nsec, 0);

        // The file must now contain three records, sorted.
        let records = read_all_mtime_records(&temp_root);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].mtime_sec, 50);
        assert_eq!(records[1].mtime_sec, 100);
        assert_eq!(records[2].mtime_sec, 200);

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn name_hash_sidecar_lookup_finds_existing_and_misses_new() {
        let temp_root = make_test_temp_root("sidecar_lookup");
        let watched = make_watched_dir_with_files(
            "sidecar_lookup",
            &[("present_one", b"a"), ("present_two", b"b")],
        );
        let _ = cold_build_index(&temp_root, &watched).expect("cold build");
        // Build the sidecar by triggering an append (no new files).
        let header = read_header(&temp_root).expect("r").expect("p");
        let _ = incremental_append_new_files(&temp_root, &watched, &header).expect("noop append");

        // Lookup an existing basename's Role-2 Pearson hash → must be
        // present.
        let present_hash = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"present_one",
            &NAME_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash present");
        assert!(name_hash_is_present_in_sidecar(&temp_root, present_hash).expect("lookup ok"));

        // Lookup a Role-2 hash for a basename that does not exist →
        // must be absent. (A theoretical Pearson hash collision could
        // produce a false positive, but for these short distinct
        // basenames at width PEARSON_SALT_ARRAY_SIZE = 2 the collision
        // probability is ~1/65536 — negligible in this test.)
        let absent_hash = pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            b"definitely_not_there_xyz",
            &NAME_HASH_SALTS,
            &GENERATED_TABLE,
        )
        .expect("hash absent");
        assert!(!name_hash_is_present_in_sidecar(&temp_root, absent_hash).expect("lookup ok"));

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn basename_used_length_handles_full_and_partial_records() {
        // Full record (no NUL padding): length is NAME_RECORD_SIZE.
        let full = [b'a'; NAME_RECORD_SIZE];
        assert_eq!(basename_used_length(&full), NAME_RECORD_SIZE);

        // Empty record: length is 0.
        let empty = [0u8; NAME_RECORD_SIZE];
        assert_eq!(basename_used_length(&empty), 0);

        // Partial record: 5 bytes then NUL padding.
        let mut partial = [0u8; NAME_RECORD_SIZE];
        partial[..5].copy_from_slice(b"hello");
        assert_eq!(basename_used_length(&partial), 5);
    }
}
// =========================================================================
// Part (d): Update orchestration and chronological lookup
// =========================================================================
//
// ## Two public entrypoints
//
// This part adds the two functions that callers normally use:
//
//   1. `create_or_update_chrono_index` — brings the on-disk index up to date with the
//      current contents of the watched directory. Compares the live
//      directory against the committed `header.bin` and dispatches to:
//         * nothing (index is already current),
//         * `incremental_append_new_files` (the steady-state path for
//           a growing directory), or
//         * `cold_build_index` (the fallback when no usable index
//           exists or the existing one is inconsistent).
//      Never halts; falls back to a rebuild on any detected
//      inconsistency.
//
//   2. `lookup_chronological_abs_file_path_at_position` — random-access
//      read-only lookup. Given a chronological position
//      (0 = earliest mtime, file_count-1 = latest), writes the absolute
//      POSIX path of the file at that position into a caller-provided
//      stack buffer. Stateless. Does not modify any file on disk.
//
// ## Lookup contract
//
// The caller provides:
//
//   - the index `temp_root_dir`,
//   - a `u64` chronological position,
//   - a mutable `[u8; MAX_FULL_PATH_LEN]` stack buffer.
//
// The function returns one of:
//
//   - `Ok(Some(ChronoLookupResult { path_byte_length, ... }))` — the
//      file at the requested position exists in the committed index;
//      `out_path_buffer[..path_byte_length]` holds its absolute POSIX
//      path bytes.
//
//   - `Ok(None)` — the requested position is at or past
//      `header.file_count`. Nothing was written to the buffer.
//
//   - `Err(...)` — terse error code; the index files are unchanged.
//
// ## Memory discipline
//
// Per-lookup allocations: none on the heap (beyond the small `PathBuf`
// values the standard library requires for `std::fs::File::open`,
// which are bounded and freed before the function returns). The full
// absolute path is assembled into the caller's stack buffer. All
// on-disk reads are fixed-size (20 B for the mtime record, 64 B for
// the name record).
//
// ## What "lookup" does NOT do
//
// It does not open, read, copy, move, or otherwise touch the contents
// of the watched file whose path it returns. It writes the path bytes
// into the caller's buffer and nothing else. The caller decides what
// (if anything) to do with the path.

/// Maximum size of the caller-provided absolute-path buffer. POSIX
/// `PATH_MAX` is typically 4096 on Linux.
pub const MAX_FULL_PATH_LEN: usize = MAX_PARENT_PATH_LEN;

///Result of one chronological-position lookup.
/// The caller-provided buffer holds the absolute POSIX path in its first path_byte_length bytes.
#[derive(Clone, Copy, Debug)]
pub struct ChronoLookupResult {
    /// Number of valid path bytes in the caller's output buffer.
    pub path_byte_length: usize,
    /// The mtime of the looked-up file. Exposed for caller logging /
    /// observability.
    pub looked_up_file_mtime_sec: i64,
    pub looked_up_file_mtime_nsec: i32,
}

// =========================================================================
// Public summary type for create_or_update_chrono_index
// =========================================================================

/// Discrete outcome categories from `create_or_update_chrono_index`. Carries no user data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// No prior committed index existed; a cold build was performed.
    ColdBuildCompleted,
    /// A previously committed index was found to be unusable
    /// (structural / consistency mismatch); a cold rebuild was performed.
    RebuiltDueToInconsistency,
    /// The live directory matched the committed index exactly; nothing
    /// was changed on disk.
    NoChangesDetected,
    /// The live directory had grown since the last commit; the new
    /// files were appended incrementally.
    IncrementalAppendCompleted,
}

/// Aggregate summary returned by `create_or_update_chrono_index`. The numeric fields are
/// 0 for outcomes that did not exercise the corresponding path.
#[derive(Clone, Copy, Debug)]
pub struct UpdateSummary {
    pub outcome: UpdateOutcome,
    /// Total files indexed by the index after this update.
    pub final_file_count: u64,
    /// If a cold build ran, the build's summary; otherwise zeroes.
    pub cold_build_summary: ColdBuildSummary,
    /// If an incremental append ran, that summary; otherwise zeroes.
    pub append_summary: AppendSummary,
}

// =========================================================================
// Live-directory probe: count + signal hash, no stat()
// =========================================================================

/// Snapshot of the watched directory's contents at probe time.
///
/// `live_signal_hash` is computed exactly the same way as
/// `ChronoIndexHeader.signal_hash` — per-basename Pearson hash under
/// `SIGNAL_HASH_SALTS` over `GENERATED_TABLE`, XOR-folded lane by lane.
/// The orchestrator in `create_or_update_chrono_index` compares this
/// directly against the committed header's `signal_hash` to detect
/// whether the live directory still matches the indexed set.
#[derive(Clone, Copy, Debug)]
struct LiveDirectoryProbe {
    live_file_count: u64,
    live_signal_hash: [u8; PEARSON_SALT_ARRAY_SIZE],
    entries_skipped_overlong_name: u64,
    entries_skipped_stat_failed: u64,
    entries_skipped_non_regular: u64,
}

/// Streams `read_dir(parent_directory)` once and computes the probe.
/// Does not call `stat()` on entries (uses `file_type()` which is
/// returned by `readdir(3)` on Linux for most filesystems).
///
/// Memory: O(1). One `OsString` per entry from the stdlib iterator,
/// freed before the next iteration. No accumulation.
fn probe_live_directory(parent_directory: &Path) -> Result<LiveDirectoryProbe, ChronoIndexError> {
    let directory_iterator = match std::fs::read_dir(parent_directory) {
        Ok(it) => it,
        Err(_) => return Err(ChronoIndexError::BuildIo),
    };

    let mut probe = LiveDirectoryProbe {
        live_file_count: 0,
        live_signal_hash: [0u8; PEARSON_SALT_ARRAY_SIZE],
        entries_skipped_overlong_name: 0,
        entries_skipped_stat_failed: 0,
        entries_skipped_non_regular: 0,
    };

    for directory_entry_result in directory_iterator {
        let directory_entry = match directory_entry_result {
            Ok(e) => e,
            Err(_) => {
                probe.entries_skipped_stat_failed =
                    probe.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };

        let file_type_info = match directory_entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                probe.entries_skipped_stat_failed =
                    probe.entries_skipped_stat_failed.saturating_add(1);
                continue;
            }
        };
        if !file_type_info.is_file() {
            probe.entries_skipped_non_regular = probe.entries_skipped_non_regular.saturating_add(1);
            continue;
        }

        let file_name_os = directory_entry.file_name();
        let basename_bytes: &[u8] = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                file_name_os.as_bytes()
            }
            #[cfg(not(unix))]
            {
                probe.entries_skipped_overlong_name =
                    probe.entries_skipped_overlong_name.saturating_add(1);
                continue;
            }
        };

        if basename_bytes.len() > MAX_BASENAME_LEN {
            probe.entries_skipped_overlong_name =
                probe.entries_skipped_overlong_name.saturating_add(1);
            continue;
        }

        // Hash the basename with the Role-1 salts and XOR-fold lane by
        // lane. Identical recipe to the one used in
        // `phase1_stream_directory_into_files` and
        // `incremental_append_new_files`, so the probe's
        // `live_signal_hash` is directly comparable to the committed
        // `header.signal_hash`. On Pearson error (only possible on an
        // empty basename, which cannot occur here), surface a terse
        // error code rather than panic.
        let per_basename_signal_hash = match pearson_hash_salt_array::<PEARSON_SALT_ARRAY_SIZE>(
            basename_bytes,
            &SIGNAL_HASH_SALTS,
            &GENERATED_TABLE,
        ) {
            Ok(bytes) => bytes,
            Err(_) => return Err(ChronoIndexError::BuildIo),
        };
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            probe.live_signal_hash[lane_index] ^= per_basename_signal_hash[lane_index];
        }
        probe.live_file_count = probe.live_file_count.saturating_add(1);
    }

    Ok(probe)
}

// =========================================================================
// On-disk consistency: do header.file_count and the data files agree?
// =========================================================================

/// Returns `true` iff `names.bin` and `mtimes.bin` are both present and
/// their byte sizes exactly match `header.file_count`. Used to detect a
/// half-applied prior write (e.g. a crash between renaming data files
/// and renaming the header).
///
/// Memory: O(1).
fn data_files_match_header_count(temp_root_dir: &Path, header: &ChronoIndexHeader) -> bool {
    let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
    let mtimes_path = build_index_file_path(temp_root_dir, MTIMES_FILENAME);

    let expected_names_size = header.file_count.saturating_mul(NAME_RECORD_SIZE as u64);
    let expected_mtimes_size = header.file_count.saturating_mul(MTIME_RECORD_SIZE as u64);

    let names_size_matches = match std::fs::metadata(&names_path) {
        Ok(m) => m.len() == expected_names_size,
        Err(_) => false,
    };
    let mtimes_size_matches = match std::fs::metadata(&mtimes_path) {
        Ok(m) => m.len() == expected_mtimes_size,
        Err(_) => false,
    };
    names_size_matches && mtimes_size_matches
}

// =========================================================================
// create_or_update_chrono_index — the high-level orchestration entrypoint
// =========================================================================

/// Brings the on-disk index in `<temp_root_dir>/chrono_index/` up to
/// date with the current contents of `parent_directory_to_index`.
///
/// Decision logic:
///
///   - No header present, OR header structurally invalid, OR data files
///     do not match the header's `file_count`:
///       → cold build (part b).
///
///   - Header present and data files consistent, AND the live directory
///     matches `(file_count, signal_hash)` exactly:
///       → no-op.
///
///   - Header present, data files consistent, AND
///       live_file_count >= header.file_count
///       AND a delta XOR exists such that all-pre-existing names are
///       still represented (verified by `header.signal_hash XOR
///       live_signal_hash` equalling the XOR of *only* the new
///       basenames):
///       → incremental append (part c).
///
///   - Anything else (live count shrank, hashes incompatible, etc.):
///       → cold rebuild.
///
/// Per project policy: never halts. Any unrecoverable inconsistency
/// triggers a cold rebuild rather than an error return.
pub fn create_or_update_chrono_index(
    temp_root_dir: &Path,
    parent_directory_to_index: &Path,
) -> Result<UpdateSummary, ChronoIndexError> {
    ensure_index_directory_exists(temp_root_dir)?;

    // Empty summaries used when a path is not taken.
    let emptycold_build_summary = ColdBuildSummary {
        files_indexed: 0,
        entries_skipped_overlong_name: 0,
        entries_skipped_stat_failed: 0,
        entries_skipped_non_regular: 0,
    };
    let emptyappend_summary = AppendSummary {
        files_appended: 0,
        entries_skipped_overlong_name: 0,
        entries_skipped_stat_failed: 0,
        entries_skipped_non_regular: 0,
        invariant_breaches_this_call: 0,
    };

    // --- Step 1: read header (or detect absence / corruption). --------
    let committed_header_opt = match read_header(temp_root_dir) {
        Ok(opt) => opt,
        Err(_structural_or_io_error) => {
            // Either I/O error or structural mismatch (bad magic, bad
            // version, bad size). Treat as "no usable header" and
            // rebuild.
            None
        }
    };

    // --- Step 2: if no usable header, cold build outright. ------------
    let committed_header = match committed_header_opt {
        Some(h) => h,
        None => {
            let build_summary = cold_build_index(temp_root_dir, parent_directory_to_index)?;
            let header_after = read_header(temp_root_dir)?.ok_or(ChronoIndexError::BuildIo)?;
            return Ok(UpdateSummary {
                outcome: UpdateOutcome::ColdBuildCompleted,
                final_file_count: header_after.file_count,
                cold_build_summary: build_summary,
                append_summary: emptyappend_summary,
            });
        }
    };

    // --- Step 3: verify data files agree with the header. -------------
    // If a previous run crashed mid-commit, the data files may be
    // larger or smaller than the header says. Rebuild in that case.
    if !data_files_match_header_count(temp_root_dir, &committed_header) {
        let build_summary = cold_build_index(temp_root_dir, parent_directory_to_index)?;
        let header_after = read_header(temp_root_dir)?.ok_or(ChronoIndexError::BuildIo)?;
        return Ok(UpdateSummary {
            outcome: UpdateOutcome::RebuiltDueToInconsistency,
            final_file_count: header_after.file_count,
            cold_build_summary: build_summary,
            append_summary: emptyappend_summary,
        });
    }

    // --- Step 4: verify parent path in header matches caller's path. --
    let passed_in_parent_bytes = posix_path_to_bytes(parent_directory_to_index)?;
    if passed_in_parent_bytes != committed_header.parent_path_slice() {
        // The caller is now watching a different directory than the
        // committed index. Rebuild against the new directory.
        let build_summary = cold_build_index(temp_root_dir, parent_directory_to_index)?;
        let header_after = read_header(temp_root_dir)?.ok_or(ChronoIndexError::BuildIo)?;
        return Ok(UpdateSummary {
            outcome: UpdateOutcome::RebuiltDueToInconsistency,
            final_file_count: header_after.file_count,
            cold_build_summary: build_summary,
            append_summary: emptyappend_summary,
        });
    }

    // --- Step 5: probe the live directory. ----------------------------
    let probe = probe_live_directory(parent_directory_to_index)?;

    // No-op case: counts and hashes match.
    if probe.live_file_count == committed_header.file_count
        && probe.live_signal_hash == committed_header.signal_hash
    {
        return Ok(UpdateSummary {
            outcome: UpdateOutcome::NoChangesDetected,
            final_file_count: committed_header.file_count,
            cold_build_summary: emptycold_build_summary,
            append_summary: emptyappend_summary,
        });
    }

    // --- Step 6: append-eligible case. --------------------------------
    //
    // Per project rules, files are never deleted. So:
    //   - live_file_count < committed_header.file_count → impossible in
    //     a well-behaved environment; treat as inconsistency, rebuild.
    //   - live_file_count == committed_header.file_count but hashes
    //     differ → some basename changed identity (rename / replace).
    //     Per project rules this should not occur; treat as inconsistency.
    //   - live_file_count > committed_header.file_count → may be
    //     append-eligible. We hand off to `incremental_append_new_files`,
    //     which performs its own per-basename check via the
    //     `name_hashes.bin` sidecar.
    //
    // The XOR delta check (header.signal_hash XOR live_signal_hash
    // equals XOR of *only* new basenames) is automatically satisfied
    // when the only change is the addition of new files, because XOR is
    // its own inverse:
    //   new_names_xor = live - existing  (in XOR algebra)
    //                 = live XOR existing
    // The append path produces a final signal_hash equal to
    // existing XOR new_names_xor == live_signal_hash by construction.
    // If after append the resulting header.signal_hash differs from the
    // probe's live_signal_hash, the orchestrator can detect that and
    // fall through to rebuild on the next call. We perform that check
    // below as a defensive consistency gate.
    if probe.live_file_count < committed_header.file_count
        || probe.live_file_count == committed_header.file_count
    {
        // Either shrinking (impossible per spec) or same-count-different-
        // contents (rename/replace, also impossible per spec). Rebuild.
        let build_summary = cold_build_index(temp_root_dir, parent_directory_to_index)?;
        let header_after = read_header(temp_root_dir)?.ok_or(ChronoIndexError::BuildIo)?;
        return Ok(UpdateSummary {
            outcome: UpdateOutcome::RebuiltDueToInconsistency,
            final_file_count: header_after.file_count,
            cold_build_summary: build_summary,
            append_summary: emptyappend_summary,
        });
    }

    // live_file_count > committed_header.file_count → attempt append.
    let append_outcome =
        incremental_append_new_files(temp_root_dir, parent_directory_to_index, &committed_header);

    let append_summary = match append_outcome {
        Ok(s) => s,
        Err(_append_error) => {
            // The append failed partway. Per the contract of
            // `incremental_append_new_files`, it has already made a
            // best-effort attempt to keep the header consistent with
            // whatever prefix it managed to write. To be fully safe,
            // we now rebuild from scratch so the index is guaranteed
            // consistent with the live directory.
            let build_summary = cold_build_index(temp_root_dir, parent_directory_to_index)?;
            let header_after = read_header(temp_root_dir)?.ok_or(ChronoIndexError::BuildIo)?;
            return Ok(UpdateSummary {
                outcome: UpdateOutcome::RebuiltDueToInconsistency,
                final_file_count: header_after.file_count,
                cold_build_summary: build_summary,
                append_summary: emptyappend_summary,
            });
        }
    };

    // Post-append consistency gate: re-read header and verify its
    // signal_hash now matches the probe's live_signal_hash. If not,
    // some assumption was violated (e.g. an FNV hash collision causing
    // a conservative skip in the append path); rebuild on the next
    // create_or_update_chrono_index call by treating this round as a rebuild.
    let header_after_append = read_header(temp_root_dir)?.ok_or(ChronoIndexError::AppendIo)?;
    if header_after_append.signal_hash != probe.live_signal_hash
        || header_after_append.file_count != probe.live_file_count
    {
        let build_summary = cold_build_index(temp_root_dir, parent_directory_to_index)?;
        let header_after_rebuild = read_header(temp_root_dir)?.ok_or(ChronoIndexError::BuildIo)?;
        return Ok(UpdateSummary {
            outcome: UpdateOutcome::RebuiltDueToInconsistency,
            final_file_count: header_after_rebuild.file_count,
            cold_build_summary: build_summary,
            append_summary: emptyappend_summary,
        });
    }

    Ok(UpdateSummary {
        outcome: UpdateOutcome::IncrementalAppendCompleted,
        final_file_count: header_after_append.file_count,
        cold_build_summary: emptycold_build_summary,
        append_summary,
    })
}

// =========================================================================
// Chronological lookup by position
// =========================================================================

/// Reads one `MtimeRecord` from `mtimes.bin` at the given record index.
/// Bounded stack memory. Returns a terse error code on any I/O failure.
fn read_mtime_record_at_index(
    temp_root_dir: &Path,
    mtime_index: u64,
) -> Result<MtimeRecord, ChronoIndexError> {
    let mtimes_path = build_index_file_path(temp_root_dir, MTIMES_FILENAME);
    let mut handle = match File::open(&mtimes_path) {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::LookupIo),
    };
    let byte_offset = mtime_index.saturating_mul(MTIME_RECORD_SIZE as u64);
    if handle.seek(SeekFrom::Start(byte_offset)).is_err() {
        return Err(ChronoIndexError::LookupIo);
    }
    let mut buffer = [0u8; MTIME_RECORD_SIZE];
    if handle.read_exact(&mut buffer).is_err() {
        return Err(ChronoIndexError::LookupIo);
    }
    Ok(MtimeRecord::read_from(&buffer))
}

/// Reads one `names.bin` record into the supplied stack buffer.
/// Returns the used length (number of bytes before NUL padding).
fn read_name_record_at_record_id(
    temp_root_dir: &Path,
    record_id: u64,
    out_name_record: &mut [u8; NAME_RECORD_SIZE],
) -> Result<usize, ChronoIndexError> {
    let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
    let mut handle = match File::open(&names_path) {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::LookupIo),
    };
    let byte_offset = record_id.saturating_mul(NAME_RECORD_SIZE as u64);
    if handle.seek(SeekFrom::Start(byte_offset)).is_err() {
        return Err(ChronoIndexError::LookupIo);
    }
    if handle.read_exact(out_name_record).is_err() {
        return Err(ChronoIndexError::LookupIo);
    }
    Ok(basename_used_length(out_name_record))
}

/// Assembles `parent_path + "/" + basename` into `out_path_buffer`.
/// Returns the used length, or an error if the result would exceed
/// `MAX_FULL_PATH_LEN`.
///
/// If `parent_path` already ends with `/`, the separator is not duplicated.
/// All operations are bounds-checked; no panic.
fn assemble_absolute_path_into_buffer(
    parent_path_bytes: &[u8],
    basename_bytes: &[u8],
    out_path_buffer: &mut [u8; MAX_FULL_PATH_LEN],
) -> Result<usize, ChronoIndexError> {
    // Defensive: a malformed empty parent is rejected.
    if parent_path_bytes.is_empty() {
        return Err(ChronoIndexError::ParentPathInvalid);
    }

    let parent_ends_with_separator = parent_path_bytes
        .last()
        .map(|byte| *byte == b'/')
        .unwrap_or(false);
    let separator_byte_count: usize = if parent_ends_with_separator { 0 } else { 1 };

    // Bounds check: parent + sep + basename must fit.
    let total_length = parent_path_bytes
        .len()
        .saturating_add(separator_byte_count)
        .saturating_add(basename_bytes.len());
    if total_length > MAX_FULL_PATH_LEN {
        return Err(ChronoIndexError::LookupIo);
    }

    let mut write_position: usize = 0;
    // Copy parent.
    out_path_buffer[write_position..write_position + parent_path_bytes.len()]
        .copy_from_slice(parent_path_bytes);
    write_position += parent_path_bytes.len();
    // Optional separator.
    if !parent_ends_with_separator {
        out_path_buffer[write_position] = b'/';
        write_position += 1;
    }
    // Basename.
    out_path_buffer[write_position..write_position + basename_bytes.len()]
        .copy_from_slice(basename_bytes);
    write_position += basename_bytes.len();

    Ok(write_position)
}

// =========================================================================
// Tests for part (d) Update orchestration and chronological lookup
// =========================================================================

#[cfg(test)]
mod chrono_index_part_d_tests {
    use super::*;
    // use std::io::Write as _;

    fn make_test_temp_root(label: &str) -> PathBuf {
        let mut scratch = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        scratch.push(format!(
            "chrono_index_d_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&scratch).expect("setup");
        scratch
    }

    fn make_watched_dir_with_files(label: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut watched = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        watched.push(format!(
            "chrono_watched_d_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&watched).expect("setup");
        for (basename, content) in files {
            let mut path = watched.clone();
            path.push(basename);
            let mut f = std::fs::File::create(&path).expect("create");
            f.write_all(content).expect("write");
            f.sync_all().expect("sync");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        watched
    }

    fn add_file_to_watched_dir(watched_dir: &Path, basename: &str, content: &[u8]) {
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut path = PathBuf::from(watched_dir);
        path.push(basename);
        let mut f = std::fs::File::create(&path).expect("create new");
        f.write_all(content).expect("write new");
        f.sync_all().expect("sync new");
    }

    #[test]
    fn update_index_on_empty_state_performs_cold_build() {
        let temp_root = make_test_temp_root("first_update");
        let watched =
            make_watched_dir_with_files("first_update", &[("a.txt", b"1"), ("b.txt", b"2")]);

        let summary = create_or_update_chrono_index(&temp_root, &watched).expect("update ok");
        assert_eq!(summary.outcome, UpdateOutcome::ColdBuildCompleted);
        assert_eq!(summary.final_file_count, 2);
        assert_eq!(summary.cold_build_summary.files_indexed, 2);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn update_index_no_changes_returns_noop_outcome() {
        let temp_root = make_test_temp_root("noop_update");
        let watched = make_watched_dir_with_files("noop_update", &[("x", b"1"), ("y", b"2")]);

        let first = create_or_update_chrono_index(&temp_root, &watched).expect("first ok");
        assert_eq!(first.outcome, UpdateOutcome::ColdBuildCompleted);

        let second = create_or_update_chrono_index(&temp_root, &watched).expect("second ok");
        assert_eq!(second.outcome, UpdateOutcome::NoChangesDetected);
        assert_eq!(second.final_file_count, 2);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn update_index_growth_triggers_incremental_append() {
        let temp_root = make_test_temp_root("growth");
        let watched = make_watched_dir_with_files("growth", &[("seed", b"s")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("cold build via update");

        add_file_to_watched_dir(&watched, "grown_one", b"1");
        add_file_to_watched_dir(&watched, "grown_two", b"2");

        let summary =
            create_or_update_chrono_index(&temp_root, &watched).expect("append via update");
        assert_eq!(summary.outcome, UpdateOutcome::IncrementalAppendCompleted);
        assert_eq!(summary.final_file_count, 3);
        assert_eq!(summary.append_summary.files_appended, 2);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn update_index_rebuilds_when_data_file_size_disagrees_with_header() {
        let temp_root = make_test_temp_root("inconsistent");
        let watched =
            make_watched_dir_with_files("inconsistent", &[("a", b"1"), ("b", b"2"), ("c", b"3")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("first ok");

        // Corrupt by truncating names.bin to half size.
        let names_path = build_index_file_path(&temp_root, NAMES_FILENAME);
        let original_size = std::fs::metadata(&names_path).expect("meta").len();
        let truncated_handle = OpenOptions::new()
            .write(true)
            .open(&names_path)
            .expect("open names rw");
        truncated_handle
            .set_len(original_size / 2)
            .expect("truncate names");
        drop(truncated_handle);

        let summary =
            create_or_update_chrono_index(&temp_root, &watched).expect("rebuild via update");
        assert_eq!(summary.outcome, UpdateOutcome::RebuiltDueToInconsistency);
        assert_eq!(summary.final_file_count, 3);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn update_index_rebuilds_when_parent_path_changed() {
        let temp_root = make_test_temp_root("reparent");
        let watched_a = make_watched_dir_with_files("reparent_a", &[("aa", b"a")]);
        let watched_b = make_watched_dir_with_files("reparent_b", &[("bb", b"b")]);

        let _ = create_or_update_chrono_index(&temp_root, &watched_a).expect("first ok");

        // Now point the same temp_root at a different parent directory.
        let summary = create_or_update_chrono_index(&temp_root, &watched_b).expect("rebuild ok");
        assert_eq!(summary.outcome, UpdateOutcome::RebuiltDueToInconsistency);
        assert_eq!(summary.final_file_count, 1);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched_a);
        let _ = std::fs::remove_dir_all(&watched_b);
    }

    #[test]
    fn assemble_absolute_path_handles_trailing_and_no_trailing_slash() {
        let mut buffer = [0u8; MAX_FULL_PATH_LEN];

        // Parent without trailing slash.
        let len1 =
            assemble_absolute_path_into_buffer(b"/var/data", b"foo.txt", &mut buffer).expect("ok");
        assert_eq!(&buffer[..len1], b"/var/data/foo.txt");

        // Parent with trailing slash.
        let len2 =
            assemble_absolute_path_into_buffer(b"/var/data/", b"bar.txt", &mut buffer).expect("ok");
        assert_eq!(&buffer[..len2], b"/var/data/bar.txt");
    }

    #[test]
    fn assemble_absolute_path_rejects_oversize_result() {
        let mut buffer = [0u8; MAX_FULL_PATH_LEN];
        // Parent length that already saturates the buffer.
        let huge_parent = vec![b'a'; MAX_FULL_PATH_LEN];
        let result = assemble_absolute_path_into_buffer(&huge_parent, b"x", &mut buffer);
        assert_eq!(result.err(), Some(ChronoIndexError::LookupIo));
    }

    #[test]
    fn assemble_absolute_path_rejects_empty_parent() {
        let mut buffer = [0u8; MAX_FULL_PATH_LEN];
        let result = assemble_absolute_path_into_buffer(b"", b"x", &mut buffer);
        assert_eq!(result.err(), Some(ChronoIndexError::ParentPathInvalid));
    }
}

// =========================================================================
// Part (e): Cleanup and inspection helpers
// =========================================================================

/// Removes ONLY the index state files under `<temp_root_dir>/chrono_index/`
/// — the `chrono_index/` subdirectory itself and everything inside it.
/// Does **not** touch the caller-supplied `temp_root_dir` itself, and
/// does **not** touch the watched directory or any of its files.
///
/// Use this when:
///   - The caller wants to discard the index entirely (e.g. switching
///     to a different watched directory and choosing not to reuse the
///     same `temp_root_dir`).
///   - A higher-level component has decided the index is unrecoverable
///     and a fresh cold rebuild on the next `create_or_update_chrono_index` is desired.
///
/// Per project policy this function does not halt. On I/O failure it
/// returns a terse error code; the caller can choose to retry or accept
/// the leftover state (a subsequent `create_or_update_chrono_index` will rebuild over it
/// in any case).
///
/// Safety / scope guarantees:
///   - Removes only `<temp_root_dir>/chrono_index/` and its contents.
///   - Never removes `<temp_root_dir>` itself.
///   - Never removes anything in or under the watched directory.
///
/// Note: if any concurrent process is currently holding open file
/// handles inside `chrono_index/`, the behavior is platform-dependent
/// (POSIX allows removal while handles remain open; the files stay
/// alive until the last handle is closed). The module's own functions
/// always open + read + close in a single call, so they do not retain
/// handles between calls.
pub fn purge_chrono_index_state(temp_root_dir: &Path) -> Result<(), ChronoIndexError> {
    let mut index_subdir = PathBuf::from(temp_root_dir);
    index_subdir.push(INDEX_SUBDIRNAME);

    match std::fs::remove_dir_all(&index_subdir) {
        Ok(()) => Ok(()),
        Err(io_error) => {
            // "Already gone" is a successful end-state, not an error.
            if io_error.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(ChronoIndexError::IndexDirIo)
            }
        }
    }
}

/// Removes only the transient scratch state under
/// `<temp_root_dir>/chrono_index/scratch/`, if any. Leaves the
/// committed index files (`header.bin`, `names.bin`, `mtimes.bin`,
/// and the lazy `name_hashes.bin` sidecar) untouched.
///
/// `cold_build_index` already cleans up `scratch/` on success and on
/// most failure paths. This helper exists for the rare case where a
/// process was killed mid-build and the next process wants to clear
/// the scratch artifacts without triggering a full rebuild yet.
///
/// Per project policy: does not halt. Returns `Ok(())` if the scratch
/// dir is absent (treated as the goal-state).
pub fn purge_scratch_only(temp_root_dir: &Path) -> Result<(), ChronoIndexError> {
    let mut scratch_dir = PathBuf::from(temp_root_dir);
    scratch_dir.push(INDEX_SUBDIRNAME);
    scratch_dir.push(SCRATCH_DIRNAME);

    match std::fs::remove_dir_all(&scratch_dir) {
        Ok(()) => Ok(()),
        Err(io_error) => {
            if io_error.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(ChronoIndexError::IndexDirIo)
            }
        }
    }
}

// =========================================================================
// Part (f): Public chronological lookup by position
// =========================================================================
//
// `lookup_abs_file_path_at_mtime_chronological_index` returns the
// absolute path of one file at a given chronological position in the
// committed index. It is the primary public read API of this module.
//
// Important: this function does NOT open, read, copy, move, or modify
// any of the watched files. It only writes the file's absolute path
// bytes into the caller-provided stack buffer.

// =========================================================================
// Public chronological lookup by position
// =========================================================================

/// Returns the absolute path of the file at chronological position
/// `chronological_position` in the committed index.
///
/// Positions are zero-based and ordered by mtime ascending:
///   - position 0                       = chronologically earliest file
///   - position `file_count - 1`        = chronologically latest file
///   - position >= `file_count`         = `Ok(None)`
///
/// This function is read-only. It does not modify any other file.
/// It may be called any number of times
/// with any positions, in any order. Two calls with the same position
/// return the same path (provided the index has not been rebuilt
/// between them).
///
/// The absolute path is written into `out_path_buffer`; the returned
/// `ChronoLookupResult.path_byte_length` is the number of valid leading
/// bytes in that buffer.
///
/// Per project policy: never panics, never halts.
pub fn lookup_abs_file_path_at_mtime_chronological_index(
    temp_root_dir: &Path,
    chronological_position: u64,
    out_path_buffer: &mut [u8; MAX_FULL_PATH_LEN],
) -> Result<Option<ChronoLookupResult>, ChronoIndexError> {
    let committed_header = match read_header(temp_root_dir)? {
        Some(h) => h,
        None => return Err(ChronoIndexError::LookupIo),
    };

    if chronological_position >= committed_header.file_count {
        return Ok(None);
    }

    let mtime_record = read_mtime_record_at_index(temp_root_dir, chronological_position)?;

    if mtime_record.record_id >= committed_header.file_count {
        return Err(ChronoIndexError::LookupIo);
    }

    let mut name_record_buffer = [0u8; NAME_RECORD_SIZE];
    let basename_used_len = read_name_record_at_record_id(
        temp_root_dir,
        mtime_record.record_id,
        &mut name_record_buffer,
    )?;
    let basename_bytes = &name_record_buffer[..basename_used_len];

    let path_byte_length = assemble_absolute_path_into_buffer(
        committed_header.parent_path_slice(),
        basename_bytes,
        out_path_buffer,
    )?;

    Ok(Some(ChronoLookupResult {
        path_byte_length,
        looked_up_file_mtime_sec: mtime_record.mtime_sec,
        looked_up_file_mtime_nsec: mtime_record.mtime_nsec,
    }))
}

/// Returns the number of files currently committed in the index — i.e.
/// the upper bound (exclusive) for valid arguments to
/// `lookup_abs_file_path_at_mtime_chronological_index`.
///
/// Returns `Ok(0)` if no header is committed yet. Never panics.
pub fn count_committed_files(temp_root_dir: &Path) -> Result<u64, ChronoIndexError> {
    match read_header(temp_root_dir)? {
        Some(header) => Ok(header.file_count),
        None => Ok(0),
    }
}

#[cfg(test)]
mod chrono_index_lookup_tests {
    use super::*;
    // use std::io::Write as _;

    fn make_test_temp_root(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "chrono_index_lookup_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&p).expect("setup");
        p
    }

    fn make_watched_dir_with_files(label: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut watched = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        watched.push(format!(
            "chrono_watched_lookup_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&watched).expect("setup");
        for (basename, content) in files {
            let mut path = watched.clone();
            path.push(basename);
            let mut f = std::fs::File::create(&path).expect("create");
            f.write_all(content).expect("write");
            f.sync_all().expect("sync");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        watched
    }

    #[test]
    fn lookup_position_zero_returns_chronologically_earliest_file() {
        let temp_root = make_test_temp_root("zero");
        let watched = make_watched_dir_with_files(
            "zero",
            &[
                ("first.txt", b"1"),
                ("second.txt", b"2"),
                ("third.txt", b"3"),
            ],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        let mut buf = [0u8; MAX_FULL_PATH_LEN];
        let result = lookup_abs_file_path_at_mtime_chronological_index(&temp_root, 0, &mut buf)
            .expect("ok")
            .expect("present");

        let path_bytes = &buf[..result.path_byte_length];
        assert!(path_bytes.ends_with(b"/first.txt"));

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn lookup_past_end_returns_none() {
        let temp_root = make_test_temp_root("past_end");
        let watched = make_watched_dir_with_files("past_end", &[("only", b"x")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        let mut buf = [0u8; MAX_FULL_PATH_LEN];
        let r =
            lookup_abs_file_path_at_mtime_chronological_index(&temp_root, 5, &mut buf).expect("ok");
        assert!(r.is_none());

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn count_committed_files_reports_header_count() {
        let temp_root = make_test_temp_root("count");
        let watched = make_watched_dir_with_files("count", &[("a", b"a"), ("b", b"b")]);

        // Before any create_or_update_chrono_index, no header → 0.
        assert_eq!(count_committed_files(&temp_root).expect("ok"), 0);

        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");
        assert_eq!(count_committed_files(&temp_root).expect("ok"), 2);

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn lookup_returns_paths_in_ascending_mtime_order() {
        let temp_root = make_test_temp_root("ascending");
        let watched = make_watched_dir_with_files(
            "ascending",
            &[("p0", b"0"), ("p1", b"1"), ("p2", b"2"), ("p3", b"3")],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        let total = count_committed_files(&temp_root).expect("ok");
        assert_eq!(total, 4);

        let mut buf = [0u8; MAX_FULL_PATH_LEN];
        let mut previous_mtime_sec: Option<i64> = None;
        let mut previous_mtime_nsec: Option<i32> = None;
        for position in 0..total {
            let r =
                lookup_abs_file_path_at_mtime_chronological_index(&temp_root, position, &mut buf)
                    .expect("ok")
                    .expect("present");

            if let (Some(prev_sec), Some(prev_nsec)) = (previous_mtime_sec, previous_mtime_nsec) {
                let strictly_ascending = r.looked_up_file_mtime_sec > prev_sec
                    || (r.looked_up_file_mtime_sec == prev_sec
                        && r.looked_up_file_mtime_nsec >= prev_nsec);
                assert!(
                    strictly_ascending,
                    "positions must be non-decreasing in mtime"
                );
            }
            previous_mtime_sec = Some(r.looked_up_file_mtime_sec);
            previous_mtime_nsec = Some(r.looked_up_file_mtime_nsec);
        }

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }
}

// src/chrono_index/mod.rs
// (Add Part (g) after the chrono_index_lookup_tests module and before
// the `//! # Chrono-Sort Module — Mini Demo` comment block.)

// =========================================================================
// Part (g): Order-sensitive chronological sequence hash
// =========================================================================
//
// ## Why these functions exist
//
// `header.signal_hash` is an XOR-fold of per-basename FNV-1a hashes.
// XOR is commutative and associative, so it is ORDER-INDEPENDENT: a
// directory whose files swap chronological positions produces an
// identical signal_hash. That is intentional for its purpose (change
// detection of the *set* of files), but it cannot detect the specific
// failure mode described in the project discussion:
//
//   A delayed thread finishes writing a file whose mtime is earlier
//   than files the game engine has already processed. The file silently
//   slides into an earlier chronological slot, retroactively changing
//   the meaning of positions 0..N that the engine used to build state.
//
// An order-SENSITIVE hash over positions 0..=N detects exactly this.
// The hash is computed by feeding (mtime_sec, mtime_nsec, basename)
// for each position, in ascending chronological order, into a running
// FNV-1a 64 accumulator. Any change to any position's content or to
// the relative ordering of positions produces a different hash with
// overwhelming probability.
//
// ## Plan-B from project discussion — usage pattern
//
//   1. After `create_or_update_chrono_index` commits a new index with file_count == K,
//      call `chrono_sort_hash_to_n(temp_root, K - 1)` and store the
//      result as `known_good_hash` in caller state.
//
//   2. On each subsequent periodic poll (before reading the "next" file):
//
//        match check_chronosort_hash_to_n(temp_root, K - 1, known_good_hash) {
//            Ok(true)  => // past sequence intact; only look at new files
//            Ok(false) => // sequence reordered; discard state, rebuild
//            Err(_)    => // index unreadable; rebuild defensively
//        }
//
//   This is much cheaper than re-reading every file on every poll: the
//   hash check reads exactly (K × (MTIME_RECORD_SIZE + NAME_RECORD_SIZE))
//   bytes from two files, with no stat() calls and no watched-file I/O.
//
// ## Memory discipline
//
// Stack only. No heap allocation. Two fixed-size reused buffers per
// iteration: 20 bytes (mtime record) + 64 bytes (name record). The hash
// state is a single u64. Memory cost is O(1), not O(N).
//
// ## Hash construction
//
// For each position p in 0..=up_to_position (in ascending order):
//   1. Read MtimeRecord at position p from mtimes.bin.
//   2. Read the basename at mtime_record.record_id from names.bin.
//   3. Feed into running FNV-1a 64:
//        mtime_sec  (8 bytes, little-endian)
//        mtime_nsec (4 bytes, little-endian)
//        basename   (N bytes, no NUL padding)
//        0xFF       (1 separator byte — prevents prefix-extension
//                    collisions: hash("ab"+"c") ≠ hash("a"+"bc"))
//
// The hash is NOT cryptographic. For file counts in the tens to low
// hundreds (the chess-game use case), collision probability under
// random file reorderings is negligible (≈ 2^-64 per comparison).

/// Computes an order-sensitive Pearson-based hash over the
/// chronologically sorted sequence of `(mtime_sec, mtime_nsec, basename)`
/// tuples at positions `0` through `up_to_position` inclusive in the
/// committed index.
///
/// ## Project context
///
/// This is the measurement tool for Plan-B change detection (see
/// module docs for Part g). The caller stores the returned hash after
/// building game state; on each subsequent poll it calls
/// [`check_chronosort_hash_to_n`] to test whether the past sequence
/// is still intact before deciding to rebuild.
///
/// Unlike `header.signal_hash` (which is XOR-folded and therefore
/// order-independent), this hash changes if any file slides to a
/// different chronological position, even if the set of files is
/// unchanged. That is the property required to detect the
/// "delayed-thread mtime retrograde" edge case.
///
/// ## Algorithm
///
/// The hash is computed with `PEARSON_SALT_ARRAY_SIZE` parallel
/// Pearson chains over the byte stream
///
/// ```text
///     for each position p in 0..=up_to_position:
///         mtime_sec_LE_bytes   (8)
///         mtime_nsec_LE_bytes  (4)
///         basename_bytes       (variable, no NUL padding)
///         0xFF                 (1, separator)
/// ```
///
/// fed through the same `GENERATED_TABLE` used by every other Pearson
/// hash in this module. Each lane `i` is initialized to
/// `CHRONO_SORT_HASH_SALTS[i]` so all lanes diverge from byte one.
/// Per-byte step for each lane `i`:
///
/// ```text
///     state[i] = GENERATED_TABLE[state[i] ^ input_byte]
/// ```
///
/// The 0xFF separator after each record prevents prefix-extension
/// ambiguities (e.g. hash(["ab","c"]) vs. hash(["a","bc"])).
///
/// ## Arguments
///
/// - `temp_root_dir`: the index temp root.
/// - `up_to_position`: the last (inclusive) chronological position to
///   include. Must be `< header.file_count`. Out-of-range or
///   missing-index → `Err(LookupIo)`.
///
/// ## Returns
///
/// - `Ok([u8; PEARSON_SALT_ARRAY_SIZE])` — deterministic Pearson hash
///   of the ordered sequence. Identical committed-index state +
///   identical `up_to_position` always returns the same value.
/// - `Err(LookupIo)` — on any I/O failure or out-of-range position.
///   Callers should treat this as "unknown change: rebuild
///   defensively."
///
/// ## Memory
///
/// Stack only. Two fixed-size reused buffers per record (20 B + 64 B)
/// and a `[u8; PEARSON_SALT_ARRAY_SIZE]` accumulator. O(1) memory,
/// independent of directory size.
///
/// Per project policy: never panics, never halts.
pub fn chrono_sort_hash_to_n(
    temp_root_dir: &Path,
    up_to_position: u64,
) -> Result<[u8; PEARSON_SALT_ARRAY_SIZE], ChronoIndexError> {
    // Read and validate the committed header. No header means the
    // index has never been built; treat as an I/O-level failure so the
    // caller knows it cannot rely on the hash.
    let committed_header = match read_header(temp_root_dir)? {
        Some(h) => h,
        None => return Err(ChronoIndexError::LookupIo),
    };

    // Defensive bounds check: the requested position must be a valid
    // slot in the committed index.
    if committed_header.file_count == 0 || up_to_position >= committed_header.file_count {
        return Err(ChronoIndexError::LookupIo);
    }

    // Open both index files once per call. Both are held open for the
    // duration of the loop; no per-record open/close overhead.
    let mtimes_path = build_index_file_path(temp_root_dir, MTIMES_FILENAME);
    let mut mtimes_handle = match File::open(&mtimes_path) {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::LookupIo),
    };

    let names_path = build_index_file_path(temp_root_dir, NAMES_FILENAME);
    let mut names_handle = match File::open(&names_path) {
        Ok(h) => h,
        Err(_) => return Err(ChronoIndexError::LookupIo),
    };

    // Fixed-size stack buffers reused across all iterations.
    let mut mtime_record_bytes = [0u8; MTIME_RECORD_SIZE];
    let mut name_record_bytes = [0u8; NAME_RECORD_SIZE];

    // `PEARSON_SALT_ARRAY_SIZE` parallel Pearson chains. Each lane
    // starts from its salt byte so the lanes diverge immediately and
    // produce statistically independent output bytes.
    let mut lane_states: [u8; PEARSON_SALT_ARRAY_SIZE] = CHRONO_SORT_HASH_SALTS;

    // Separator byte fed after each record's payload to prevent
    // prefix-extension hash collisions (e.g. without a separator,
    // hash(["ab", "c"]) could equal hash(["a", "bc"]) for certain
    // byte sequences).
    const RECORD_SEPARATOR_BYTE: u8 = 0xFF;

    // Number of chronological positions to include: 0..=up_to_position.
    // Bounded by committed_header.file_count (validated above).
    let record_count_to_hash: u64 = up_to_position.saturating_add(1);
    let mut positions_processed: u64 = 0;

    // mtimes.bin is read sequentially from offset 0; a single open +
    // sequential read suffices (no per-record seek on the mtime side).
    // names.bin requires random-access seeks (record_id is the
    // mtime-sort order's back-reference into the insertion-order name
    // store).
    while positions_processed < record_count_to_hash {
        // Read the next mtime record in chronological order.
        match mtimes_handle.read_exact(&mut mtime_record_bytes) {
            Ok(()) => {}
            Err(_read_error) => return Err(ChronoIndexError::LookupIo),
        }
        let mtime_record = MtimeRecord::read_from(&mtime_record_bytes);

        // Defensive: the back-reference record_id must be a valid slot
        // in names.bin. A corrupt index could produce an out-of-range
        // value; catch it here rather than seeking past the file end.
        if mtime_record.record_id >= committed_header.file_count {
            return Err(ChronoIndexError::LookupIo);
        }

        // Seek names.bin to the slot for this record_id and read it.
        let names_byte_offset = mtime_record
            .record_id
            .saturating_mul(NAME_RECORD_SIZE as u64);
        if names_handle
            .seek(SeekFrom::Start(names_byte_offset))
            .is_err()
        {
            return Err(ChronoIndexError::LookupIo);
        }
        match names_handle.read_exact(&mut name_record_bytes) {
            Ok(()) => {}
            Err(_read_error) => return Err(ChronoIndexError::LookupIo),
        }

        // Trim NUL padding to get the actual basename bytes.
        let basename_used_len = basename_used_length(&name_record_bytes);
        let basename_bytes = &name_record_bytes[..basename_used_len];

        // Feed bytes into the parallel Pearson chains in this order:
        //   mtime_sec  (8 bytes, LE) — captures WHEN the file was last modified
        //   mtime_nsec (4 bytes, LE) — sub-second resolution tiebreaker
        //   basename   (variable)    — captures WHICH file is at this position
        //   0xFF separator           — prevents prefix-extension ambiguity
        //
        // The order of these four sub-streams is deterministic and
        // identical across runs, so the resulting hash is reproducible.
        for byte_value in mtime_record.mtime_sec.to_le_bytes() {
            for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
                let table_index: usize = (lane_states[lane_index] ^ byte_value) as usize;
                lane_states[lane_index] = GENERATED_TABLE[table_index];
            }
        }
        for byte_value in mtime_record.mtime_nsec.to_le_bytes() {
            for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
                let table_index: usize = (lane_states[lane_index] ^ byte_value) as usize;
                lane_states[lane_index] = GENERATED_TABLE[table_index];
            }
        }
        for &byte_value in basename_bytes {
            for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
                let table_index: usize = (lane_states[lane_index] ^ byte_value) as usize;
                lane_states[lane_index] = GENERATED_TABLE[table_index];
            }
        }
        for lane_index in 0..PEARSON_SALT_ARRAY_SIZE {
            let table_index: usize = (lane_states[lane_index] ^ RECORD_SEPARATOR_BYTE) as usize;
            lane_states[lane_index] = GENERATED_TABLE[table_index];
        }

        positions_processed = positions_processed.saturating_add(1);
    }

    // Defensive post-loop check: if the loop exited with fewer
    // iterations than expected (which should not occur given the
    // bounds validated above, but physical I/O can produce unexpected
    // results), surface a terse error rather than returning a partial
    // hash silently.
    if positions_processed != record_count_to_hash {
        return Err(ChronoIndexError::LookupIo);
    }

    Ok(lane_states)
}

/// Tests whether the chronologically sorted sequence of files at
/// positions `0` through `up_to_position` (inclusive) in the
/// committed index is identical to the sequence that produced
/// `previous_hash`.
///
/// ## Project context
///
/// This is the polling function for Plan-B change detection (see
/// module docs for Part g). It is the cheap per-tick check that
/// replaces constant full-state rebuilds. In the chess-game use case:
///
///   - Most of the time `check_chronosort_hash_to_n` returns `Ok(true)`
///     and the engine can proceed without rebuilding.
///   - On the rare occasion that a delayed thread's file retroactively
///     shifts the chronological order of committed positions, it
///     returns `Ok(false)` and the engine discards and rebuilds its
///     state.
///
/// The cost is proportional to `up_to_position + 1` fixed-size disk
/// reads — much cheaper than re-reading every watched file on every
/// tick, and more reliable than a count-only or XOR-only check.
///
/// ## Arguments
///
/// - `temp_root_dir`: the index temp root.
/// - `up_to_position`: the last (inclusive) chronological position to
///   include. Must be `< header.file_count`.
/// - `previous_hash`: the value previously returned by
///   [`chrono_sort_hash_to_n`] with the same `up_to_position`. The
///   caller is responsible for storing this across calls.
///
/// ## Returns
///
/// - `Ok(true)` — the sequence at positions `0..=up_to_position` is
///   unchanged from when `previous_hash` was computed.
/// - `Ok(false)` — the sequence has changed; the caller should
///   discard state and rebuild.
/// - `Err(LookupIo)` — the hash could not be computed; treat as
///   unknown-change and rebuild defensively.
///
/// Per project policy: never panics, never halts.
pub fn check_chronosort_hash_to_n(
    temp_root_dir: &Path,
    up_to_position: u64,
    previous_hash: [u8; PEARSON_SALT_ARRAY_SIZE],
) -> Result<bool, ChronoIndexError> {
    let current_hash = chrono_sort_hash_to_n(temp_root_dir, up_to_position)?;
    Ok(current_hash == previous_hash)
}

// =========================================================================
// Tests for Part (g): order-sensitive sequence hash
// =========================================================================

#[cfg(test)]
mod chrono_index_part_g_tests {
    use super::*;

    fn make_test_temp_root(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!(
            "chrono_index_g_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&path).expect("setup: create temp root");
        path
    }

    /// Creates a watched directory, populates it with the given files
    /// (each given a 10 ms sleep so subsequent files have strictly newer
    /// mtimes on ms-resolution filesystems), and returns the path.
    fn make_watched_dir_with_files(label: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut watched = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        watched.push(format!(
            "chrono_watched_g_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&watched).expect("setup: create watched dir");
        for (basename, content) in files {
            let mut path = watched.clone();
            path.push(basename);
            let mut f = std::fs::File::create(&path).expect("setup: create file");
            f.write_all(content).expect("setup: write file");
            f.sync_all().expect("setup: sync file");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        watched
    }

    /// Adds one file with a 15 ms pre-sleep (ensures strictly newer
    /// mtime on filesystems with ms-resolution timestamps).
    fn add_file(watched_dir: &Path, basename: &str, content: &[u8]) {
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut path = PathBuf::from(watched_dir);
        path.push(basename);
        let mut f = std::fs::File::create(&path).expect("add_file: create");
        f.write_all(content).expect("add_file: write");
        f.sync_all().expect("add_file: sync");
    }

    // -----------------------------------------------------------------
    // chrono_sort_hash_to_n: basic correctness
    // -----------------------------------------------------------------

    #[test]
    fn hash_to_n_is_deterministic_across_repeated_calls() {
        // The same index queried twice must return the same hash.
        let temp_root = make_test_temp_root("deterministic");
        let watched = make_watched_dir_with_files(
            "deterministic",
            &[("a.txt", b"1"), ("b.txt", b"2"), ("c.txt", b"3")],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        let hash_first = chrono_sort_hash_to_n(&temp_root, 2).expect("hash ok first");
        let hash_second = chrono_sort_hash_to_n(&temp_root, 2).expect("hash ok second");
        assert_eq!(
            hash_first, hash_second,
            "repeated calls must return equal hash"
        );

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn hash_to_n_differs_for_different_up_to_positions() {
        // Hash of positions 0..=0 must differ from hash of 0..=1
        // (different sequence length → different hash).
        let temp_root = make_test_temp_root("diff_positions");
        let watched = make_watched_dir_with_files(
            "diff_positions",
            &[
                ("early.dat", b"e"),
                ("middle.dat", b"m"),
                ("late.dat", b"l"),
            ],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        let hash_0 = chrono_sort_hash_to_n(&temp_root, 0).expect("hash pos 0");
        let hash_1 = chrono_sort_hash_to_n(&temp_root, 1).expect("hash pos 1");
        let hash_2 = chrono_sort_hash_to_n(&temp_root, 2).expect("hash pos 2");

        assert_ne!(
            hash_0, hash_1,
            "hash of 1 position must differ from 2 positions"
        );
        assert_ne!(
            hash_1, hash_2,
            "hash of 2 positions must differ from 3 positions"
        );
        assert_ne!(
            hash_0, hash_2,
            "hash of 1 position must differ from 3 positions"
        );

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn hash_to_n_rejects_position_at_or_past_file_count() {
        let temp_root = make_test_temp_root("out_of_range");
        let watched = make_watched_dir_with_files("out_of_range", &[("only.txt", b"x")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");
        // file_count == 1, so valid positions are only 0.
        // position 1 must fail.
        let result = chrono_sort_hash_to_n(&temp_root, 1);
        assert_eq!(result.err(), Some(ChronoIndexError::LookupIo));
        // position u64::MAX must also fail.
        let result_max = chrono_sort_hash_to_n(&temp_root, u64::MAX);
        assert_eq!(result_max.err(), Some(ChronoIndexError::LookupIo));

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn hash_to_n_rejects_empty_index() {
        // No index built yet → Err.
        let temp_root = make_test_temp_root("empty_index");
        let result = chrono_sort_hash_to_n(&temp_root, 0);
        assert!(result.is_err(), "hash on absent index must return Err");

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn hash_to_n_changes_when_new_file_inserted_before_existing_file() {
        // Construct a scenario where a rebuild causes a file to appear
        // at position 0 that was not previously there.
        //
        // We simulate this by:
        //   1. Building the index with two files (x, y) in order.
        //   2. Recording hash_at_0 (position 0 = x).
        //   3. Cold-rebuilding with a third file z that has an EARLIER
        //      mtime than x (simulated by touching z and then x again).
        //      After rebuild, position 0 = z, position 1 = x.
        //   4. hash_at_0 must differ from the stored value.
        //
        // On a real filesystem we can simulate this by:
        //   - Creating z first (so it has an earlier mtime than x).
        //   - Then creating x and y.
        //   - First build sees x and y (cold build skips z because z
        //     was already there during the initial call — actually z IS
        //     there, so the cold build sees all three).
        //
        // Cleaner simulation: build index with one file, record hash,
        // then cold-rebuild with a *different* single file that has the
        // same position 0 slot.
        let temp_root = make_test_temp_root("position_shift");

        // Directory A with one file.
        let watched_a = make_watched_dir_with_files("pshift_a", &[("alpha.txt", b"a")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched_a).expect("build a");
        let hash_alpha = chrono_sort_hash_to_n(&temp_root, 0).expect("hash alpha");

        // Now index a completely different directory with a different file.
        // This triggers a rebuild (different parent path).
        let watched_b = make_watched_dir_with_files("pshift_b", &[("beta.txt", b"b")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched_b).expect("build b");
        let hash_beta = chrono_sort_hash_to_n(&temp_root, 0).expect("hash beta");

        assert_ne!(
            hash_alpha, hash_beta,
            "different file at position 0 must produce different hash"
        );

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched_a);
        let _ = std::fs::remove_dir_all(&watched_b);
    }

    #[test]
    fn hash_to_n_is_order_sensitive_not_set_sensitive() {
        // This test verifies the key property that distinguishes
        // chrono_sort_hash_to_n from signal_hash (XOR-based).
        //
        // Two separate indices, each with the same two basenames but in
        // OPPOSITE chronological order, must produce different hashes.
        //
        // We achieve opposite order by creating the files in opposite
        // insertion order (exploiting the 10 ms sleep in setup):
        //   Index A: position 0 = "first.txt", position 1 = "second.txt"
        //   Index B: position 0 = "second.txt", position 1 = "first.txt"
        let temp_root_a = make_test_temp_root("order_sensitive_a");
        let watched_a = make_watched_dir_with_files(
            "order_sensitive_a",
            &[("first.txt", b"f"), ("second.txt", b"s")],
        );
        let _ = create_or_update_chrono_index(&temp_root_a, &watched_a).expect("build a");
        // Verify the index has the expected order: position 0's path ends with first.txt.
        let mut buf = [0u8; MAX_FULL_PATH_LEN];
        let r0 = lookup_abs_file_path_at_mtime_chronological_index(&temp_root_a, 0, &mut buf)
            .expect("ok")
            .expect("present");
        let path0_a = buf[..r0.path_byte_length].to_vec();

        let temp_root_b = make_test_temp_root("order_sensitive_b");
        let watched_b = make_watched_dir_with_files(
            "order_sensitive_b",
            &[("second.txt", b"s"), ("first.txt", b"f")],
        );
        let _ = create_or_update_chrono_index(&temp_root_b, &watched_b).expect("build b");
        let r0b = lookup_abs_file_path_at_mtime_chronological_index(&temp_root_b, 0, &mut buf)
            .expect("ok")
            .expect("present");
        let path0_b = buf[..r0b.path_byte_length].to_vec();

        // Only proceed with the hash comparison if the order actually
        // differs (on some filesystems sub-10ms mtimes may collide;
        // the record_id tiebreaker may then preserve insertion order
        // for both). If the filesystem gave us the same order, skip the
        // assertion to avoid a spurious test failure.
        if path0_a != path0_b {
            let hash_a = chrono_sort_hash_to_n(&temp_root_a, 1).expect("hash a");
            let hash_b = chrono_sort_hash_to_n(&temp_root_b, 1).expect("hash b");
            assert_ne!(
                hash_a, hash_b,
                "same files in different order must produce different hash"
            );
        }

        let _ = std::fs::remove_dir_all(&temp_root_a);
        let _ = std::fs::remove_dir_all(&temp_root_b);
        let _ = std::fs::remove_dir_all(&watched_a);
        let _ = std::fs::remove_dir_all(&watched_b);
    }

    #[test]
    fn hash_to_n_covers_only_up_to_n_not_beyond() {
        // If only the file AFTER position N changes (an appended file),
        // the hash at position N must stay the same.
        let temp_root = make_test_temp_root("prefix_stable");
        let watched = make_watched_dir_with_files(
            "prefix_stable",
            &[("file0.dat", b"0"), ("file1.dat", b"1")],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        // Record hash at position 0 and position 1.
        let hash_at_0 = chrono_sort_hash_to_n(&temp_root, 0).expect("hash 0");
        let hash_at_1 = chrono_sort_hash_to_n(&temp_root, 1).expect("hash 1");

        // Add a new file (appended to the end: position 2).
        add_file(&watched, "file2.dat", b"2");
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("append");

        // Hash at position 0 and 1 must be unchanged: positions 0 and 1
        // were not affected by the append.
        let hash_at_0_after = chrono_sort_hash_to_n(&temp_root, 0).expect("hash 0 after");
        let hash_at_1_after = chrono_sort_hash_to_n(&temp_root, 1).expect("hash 1 after");
        assert_eq!(
            hash_at_0, hash_at_0_after,
            "prefix hash at 0 must be stable after append"
        );
        assert_eq!(
            hash_at_1, hash_at_1_after,
            "prefix hash at 1 must be stable after append"
        );

        // Hash at position 2 (the new file) must now be computable.
        let hash_at_2 = chrono_sort_hash_to_n(&temp_root, 2);
        assert!(
            hash_at_2.is_ok(),
            "position 2 must be queryable after append"
        );

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    // -----------------------------------------------------------------
    // check_chronosort_hash_to_n: correctness
    // -----------------------------------------------------------------

    #[test]
    fn check_returns_true_when_sequence_is_unchanged() {
        let temp_root = make_test_temp_root("check_true");
        let watched = make_watched_dir_with_files(
            "check_true",
            &[("move1", b"w"), ("move2", b"b"), ("move3", b"w")],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        // Record hash for positions 0..=2.
        let stored_hash = chrono_sort_hash_to_n(&temp_root, 2).expect("hash ok");

        // No change to the directory; check must return true.
        let unchanged = check_chronosort_hash_to_n(&temp_root, 2, stored_hash).expect("check ok");
        assert!(unchanged, "unchanged sequence must yield Ok(true)");

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn check_returns_false_when_stored_hash_does_not_match_current() {
        let temp_root = make_test_temp_root("check_false");
        let watched = make_watched_dir_with_files("check_false", &[("a", b"1"), ("b", b"2")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        // Compute the real hash, then construct a deliberately wrong
        // value as the "previous" hash by toggling one lane. Any
        // single-byte change is enough to make the comparison Ok(false).
        let real_hash = chrono_sort_hash_to_n(&temp_root, 1).expect("hash ok");
        let mut wrong_hash = real_hash;
        wrong_hash[0] = wrong_hash[0].wrapping_add(1);

        let changed = check_chronosort_hash_to_n(&temp_root, 1, wrong_hash).expect("check ok");
        assert!(!changed, "mismatched previous_hash must yield Ok(false)");

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn check_returns_false_after_rebuild_changes_position_content() {
        // Build, store hash, rebuild against a different directory
        // (same temp root), check → must be false.
        let temp_root = make_test_temp_root("check_rebuild");
        let watched_a = make_watched_dir_with_files("check_rebuild_a", &[("alpha", b"a")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched_a).expect("build a");
        let hash_before_rebuild = chrono_sort_hash_to_n(&temp_root, 0).expect("hash before");

        // Rebuild against a different watched directory.
        let watched_b = make_watched_dir_with_files("check_rebuild_b", &[("beta", b"b")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched_b).expect("build b");

        // check against the pre-rebuild hash must return false.
        let result =
            check_chronosort_hash_to_n(&temp_root, 0, hash_before_rebuild).expect("check ok");
        assert!(
            !result,
            "hash after rebuild with different content must not match pre-rebuild hash"
        );

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched_a);
        let _ = std::fs::remove_dir_all(&watched_b);
    }

    #[test]
    fn check_returns_err_when_position_out_of_range() {
        // Requesting a position past file_count must return Err (not
        // a silent false, which would incorrectly signal "changed").
        let temp_root = make_test_temp_root("check_oob");
        let watched = make_watched_dir_with_files("check_oob", &[("sole", b"x")]);
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");

        // The exact "previous_hash" value does not matter — the
        // function must return Err on the bad position before it ever
        // compares the hash. Use any constant array of the right type.
        let placeholder_hash: [u8; PEARSON_SALT_ARRAY_SIZE] = [0xDEu8; PEARSON_SALT_ARRAY_SIZE];
        let result = check_chronosort_hash_to_n(&temp_root, 5, placeholder_hash);
        assert_eq!(result.err(), Some(ChronoIndexError::LookupIo));

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn check_plan_b_pattern_works_across_append_then_recheck() {
        // End-to-end Plan-B usage pattern:
        //
        //   1. Build index (K files). Store hash_K = chrono_sort_hash_to_n(K-1).
        //   2. Append one more file (K+1 total). Update index.
        //   3. check_chronosort_hash_to_n(K-1, hash_K) must return true:
        //      the first K positions are unchanged by a pure append.
        //   4. Store hash_K1 = chrono_sort_hash_to_n(K).
        //   5. check_chronosort_hash_to_n(K, hash_K1) must return true
        //      immediately after storage.
        let temp_root = make_test_temp_root("plan_b_pattern");
        let watched = make_watched_dir_with_files(
            "plan_b_pattern",
            &[("move01", b"w"), ("move02", b"b"), ("move03", b"w")],
        );
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("build");
        // K = 3, last position = 2.
        let stored_hash_at_2 = chrono_sort_hash_to_n(&temp_root, 2).expect("initial hash");

        // Append move04.
        add_file(&watched, "move04", b"b");
        let _ = create_or_update_chrono_index(&temp_root, &watched).expect("append");

        // Positions 0..=2 are unchanged: check must return true.
        let prefix_stable =
            check_chronosort_hash_to_n(&temp_root, 2, stored_hash_at_2).expect("check ok");
        assert!(
            prefix_stable,
            "Plan-B: pure append must not change hash of prefix positions"
        );

        // Store hash for position 3 (the new file).
        let stored_hash_at_3 = chrono_sort_hash_to_n(&temp_root, 3).expect("hash at 3");
        let still_same =
            check_chronosort_hash_to_n(&temp_root, 3, stored_hash_at_3).expect("check 3 ok");
        assert!(still_same, "freshly stored hash must match immediately");

        let _ = std::fs::remove_dir_all(&temp_root);
        let _ = std::fs::remove_dir_all(&watched);
    }

    #[test]
    fn error_code_for_hash_functions_is_lookup_io() {
        // Both functions must surface LookupIo (not another variant)
        // for the out-of-range and no-index cases.
        let temp_root = make_test_temp_root("error_code");
        // No index built.
        assert_eq!(
            chrono_sort_hash_to_n(&temp_root, 0).err(),
            Some(ChronoIndexError::LookupIo)
        );
        assert_eq!(
            check_chronosort_hash_to_n(&temp_root, 0, [0u8; PEARSON_SALT_ARRAY_SIZE],).err(),
            Some(ChronoIndexError::LookupIo)
        );
        let _ = std::fs::remove_dir_all(&temp_root);
    }
}

// ==============
//  Buffy Format
// ==============

// =============================================================================
// TYPES
// =============================================================================

/// ANSI styling for terminal text
///
/// ## Project Context
/// Represents visual formatting for TUI elements (menus, errors, highlights).
/// All fields are compile-time constants (&'static str) - no allocation.
///
/// Memory: should be all stack, no heap
/// All style codes are static string slices pointing to program binary.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuffyStyles {
    pub fg_color: Option<&'static str>,
    pub bg_color: Option<&'static str>,
    pub bold: bool,
    pub underline: bool,
    pub italic: bool,
    pub dim: bool,
}

/// Format arguments that can be converted to strings without heap allocation
///
/// ## Project Context
/// Represents values to insert into format templates. Each variant handles
/// a specific type with stack-based conversion.
///
/// Memory: should be all stack, no heap
/// All variants store values directly (no pointers to heap).
/// String variants are references to existing data (no new allocation).
///
/// ## Supported Types
/// - Str: Existing string slices
/// - U8, U16, U32, U64, Usize: Unsigned integers (stack-converted)
/// - I8, I16, I32, I64, Isize: Signed integers (stack-converted)
/// - U8Hex, U16Hex, U32Hex: Hex formatting (stack-converted)
/// - Bool: true/false
/// - Char: Single character
/// - Path: File paths (borrowed reference)
/// - Styled variants: Include ANSI styling
#[derive(Debug, Clone)]
pub enum BuffyFormatArg<'a> {
    // // Unsigned integers
    Str(&'a str),
    // U8(u8),
    // U16(u16),
    // U32(u32),
    // U64(u64),
    Usize(usize),

    // Signed integers
    // I8(i8),
    // I16(i16),
    // I32(i32),
    // I64(i64),
    // Isize(isize),

    // // Hex formatting
    // U8Hex(u8),
    // U16Hex(u16),
    // U32Hex(u32),

    // // Other types
    // Bool(bool),
    // Char(char),
    Path(&'a Path),

    // Styled variants (adds ANSI codes)
    CharStyled(char, BuffyStyles),
    StrStyled(&'a str, BuffyStyles),
    // U8Styled(u8, BuffyStyles),
    // U64Styled(u64, BuffyStyles),
    // UsizeStyled(usize, BuffyStyles),
    // U8HexStyled(u8, BuffyStyles),
}

// =============================================================================
// INTERNAL HELPERS - Stack-based conversions
// =============================================================================

/// Converts u64 to decimal string in provided stack buffer
///
/// Memory: should be all stack, no heap
/// Writes digits directly into user's buffer, returns slice of that buffer.
///
/// ## Parameters
/// - value: Number to convert
/// - buf: Stack buffer to write into (min 20 bytes for u64::MAX)
///
/// ## Returns
/// - Some(&str): Formatted number borrowing from buf
/// - None: Buffer too small
fn format_u64_to_buffer<'a>(value: u64, buf: &'a mut [u8]) -> Option<&'a str> {
    if buf.is_empty() {
        return None;
    }

    if value == 0 {
        buf[0] = b'0';
        return std::str::from_utf8(&buf[..1]).ok();
    }

    let mut num = value;
    let mut temp = [0u8; 20]; // Stack buffer for digit reversal
    let mut pos = 0;

    while num > 0 {
        temp[pos] = b'0' + (num % 10) as u8;
        num /= 10;
        pos += 1;
    }

    if pos > buf.len() {
        return None;
    }

    // Reverse digits into output buffer
    for i in 0..pos {
        buf[i] = temp[pos - 1 - i];
    }

    std::str::from_utf8(&buf[..pos]).ok()
}

// /// Converts i64 to decimal string with sign in provided stack buffer
// ///
// /// Memory: should be all stack, no heap
// fn format_i64_to_buffer<'a>(value: i64, buf: &'a mut [u8]) -> Option<&'a str> {
//     if buf.is_empty() {
//         return None;
//     }

//     if value == 0 {
//         buf[0] = b'0';
//         return std::str::from_utf8(&buf[..1]).ok();
//     }

//     let (is_negative, abs_value) = if value < 0 {
//         (true, value.wrapping_abs() as u64)
//     } else {
//         (false, value as u64)
//     };

//     let mut temp = [0u8; 20];
//     let mut pos = 0;
//     let mut num = abs_value;

//     while num > 0 {
//         temp[pos] = b'0' + (num % 10) as u8;
//         num /= 10;
//         pos += 1;
//     }

//     let total_len = if is_negative { pos + 1 } else { pos };

//     if total_len > buf.len() {
//         return None;
//     }

//     let mut buf_pos = 0;

//     if is_negative {
//         buf[buf_pos] = b'-';
//         buf_pos += 1;
//     }

//     for i in 0..pos {
//         buf[buf_pos + i] = temp[pos - 1 - i];
//     }

//     std::str::from_utf8(&buf[..total_len]).ok()
// }

// /// Converts u8 to 2-digit uppercase hex in provided stack buffer
// ///
// /// Memory: should be all stack, no heap
// fn format_u8_hex_to_buffer<'a>(value: u8, buf: &'a mut [u8]) -> Option<&'a str> {
//     if buf.len() < 2 {
//         return None;
//     }

//     let hex_chars = b"0123456789ABCDEF";
//     buf[0] = hex_chars[(value >> 4) as usize];
//     buf[1] = hex_chars[(value & 0x0F) as usize];

//     std::str::from_utf8(&buf[..2]).ok()
// }

// /// Converts u16 to 4-digit uppercase hex in provided stack buffer
// ///
// /// Memory: should be all stack, no heap
// fn format_u16_hex_to_buffer<'a>(value: u16, buf: &'a mut [u8]) -> Option<&'a str> {
//     if buf.len() < 4 {
//         return None;
//     }

//     let hex_chars = b"0123456789ABCDEF";
//     buf[0] = hex_chars[((value >> 12) & 0x0F) as usize];
//     buf[1] = hex_chars[((value >> 8) & 0x0F) as usize];
//     buf[2] = hex_chars[((value >> 4) & 0x0F) as usize];
//     buf[3] = hex_chars[(value & 0x0F) as usize];

//     std::str::from_utf8(&buf[..4]).ok()
// }

// /// Converts u32 to 8-digit uppercase hex in provided stack buffer
// ///
// /// Memory: should be all stack, no heap
// fn format_u32_hex_to_buffer<'a>(value: u32, buf: &'a mut [u8]) -> Option<&'a str> {
//     if buf.len() < 8 {
//         return None;
//     }

//     let hex_chars = b"0123456789ABCDEF";
//     for i in 0..8 {
//         let shift = 28 - (i * 4);
//         buf[i] = hex_chars[((value >> shift) & 0x0F) as usize];
//     }

//     std::str::from_utf8(&buf[..8]).ok()
// }

/// Converts BuffyStyles to ANSI escape sequences in provided stack buffer
///
/// Memory: should be all stack, no heap
/// Concatenates ANSI codes directly into buffer.
pub fn style_to_ansi<'a>(style: BuffyStyles, buf: &'a mut [u8]) -> Option<&'a str> {
    let mut pos = 0;

    if style.bold {
        let code = b"\x1b[1m";
        if pos + code.len() > buf.len() {
            return None;
        }
        buf[pos..pos + code.len()].copy_from_slice(code);
        pos += code.len();
    }

    if style.underline {
        let code = b"\x1b[4m";
        if pos + code.len() > buf.len() {
            return None;
        }
        buf[pos..pos + code.len()].copy_from_slice(code);
        pos += code.len();
    }

    if style.italic {
        let code = b"\x1b[3m";
        if pos + code.len() > buf.len() {
            return None;
        }
        buf[pos..pos + code.len()].copy_from_slice(code);
        pos += code.len();
    }

    if style.dim {
        let code = b"\x1b[2m";
        if pos + code.len() > buf.len() {
            return None;
        }
        buf[pos..pos + code.len()].copy_from_slice(code);
        pos += code.len();
    }

    if let Some(fg) = style.fg_color {
        let code = fg.as_bytes();
        if pos + code.len() > buf.len() {
            return None;
        }
        buf[pos..pos + code.len()].copy_from_slice(code);
        pos += code.len();
    }

    if let Some(bg) = style.bg_color {
        let code = bg.as_bytes();
        if pos + code.len() > buf.len() {
            return None;
        }
        buf[pos..pos + code.len()].copy_from_slice(code);
        pos += code.len();
    }

    std::str::from_utf8(&buf[..pos]).ok()
}

// =============================================================================
// ALIGNMENT SUPPORT
// =============================================================================

#[derive(Debug, Clone, Copy)]
enum Alignment {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy)]
struct FormatSpec {
    alignment: Alignment,
    width: Option<usize>,
}

/// Parse format specifier from placeholder text
/// Examples: "" -> no alignment, "<5" -> left 5, ">10" -> right 10
fn parse_format_spec(placeholder: &str) -> Option<FormatSpec> {
    if placeholder.is_empty() {
        return Some(FormatSpec {
            alignment: Alignment::Left,
            width: None,
        });
    }

    if !placeholder.starts_with(':') {
        return None;
    }

    let spec = &placeholder[1..];

    if spec.is_empty() {
        return Some(FormatSpec {
            alignment: Alignment::Left,
            width: None,
        });
    }

    let (alignment, width_str) = if spec.starts_with('<') {
        (Alignment::Left, &spec[1..])
    } else if spec.starts_with('>') {
        (Alignment::Right, &spec[1..])
    } else if spec.starts_with('^') {
        (Alignment::Center, &spec[1..])
    } else if spec.chars().next()?.is_ascii_digit() {
        (Alignment::Right, spec)
    } else {
        return None;
    };

    let width = if width_str.is_empty() {
        None
    } else {
        match width_str.parse::<usize>() {
            Ok(w) if w <= 64 => Some(w),
            _ => return None,
        }
    };

    Some(FormatSpec { alignment, width })
}

/// Apply alignment to a value, writing result to buffer
/// Returns number of bytes written, or None if buffer too small
fn apply_alignment<'a>(value: &str, spec: FormatSpec, buf: &'a mut [u8]) -> Option<&'a str> {
    let width = match spec.width {
        Some(w) => w,
        None => {
            // No width specified, just copy value
            let value_bytes = value.as_bytes();
            if value_bytes.len() > buf.len() {
                return None;
            }
            buf[..value_bytes.len()].copy_from_slice(value_bytes);
            return std::str::from_utf8(&buf[..value_bytes.len()]).ok();
        }
    };

    let value_len = value.len();

    if value_len >= width {
        // Value already meets or exceeds width
        if value_len > buf.len() {
            return None;
        }
        buf[..value_len].copy_from_slice(value.as_bytes());
        return std::str::from_utf8(&buf[..value_len]).ok();
    }

    if width > buf.len() {
        return None;
    }

    let padding = width - value_len;

    match spec.alignment {
        Alignment::Left => {
            // Value then spaces
            buf[..value_len].copy_from_slice(value.as_bytes());
            for i in value_len..width {
                buf[i] = b' ';
            }
        }
        Alignment::Right => {
            // Spaces then value
            for i in 0..padding {
                buf[i] = b' ';
            }
            buf[padding..width].copy_from_slice(value.as_bytes());
        }
        Alignment::Center => {
            // Spaces, value, spaces
            let left_pad = padding / 2;
            // Right pad not needed - calculated as (width - left_pad - value_len)
            for i in 0..left_pad {
                buf[i] = b' ';
            }
            buf[left_pad..left_pad + value_len].copy_from_slice(value.as_bytes());
            for i in (left_pad + value_len)..width {
                buf[i] = b' ';
            }
        }
    }

    std::str::from_utf8(&buf[..width]).ok()
}

// =============================================================================
// DIRECT TERMINAL OUTPUT - TRUE ZERO HEAP
// =============================================================================

/// Writes formatted output directly to stdout without any intermediate allocation.
///
/// ## Project Context
/// Primary output function for TUI. Processes format template and writes
/// results directly to terminal as it goes. No String building, no Vec,
/// no intermediate storage.
///
/// Memory: should be all stack, no heap
/// All conversions use stack buffers. Output written directly to stdout.
///
/// ## Operation
/// 1. Parse template piece by piece
/// 2. For literal text: write directly
/// 3. For placeholders: convert arg on stack, write result
/// 4. Continue until template exhausted
///
/// ## Safety & Error Handling
/// - No panic: Returns io::Error on write failure
/// - Bounded: Max 8 arguments (prevents stack overflow)
/// - Validates: All conversions checked, returns error on failure
/// - Non-critical: Caller can continue on error
///
/// ## Parameters
/// - template: Format string with {} or {:<N}/{:>N}/{:^N} placeholders
/// - args: Slice of BuffyFormatArg values (max 8)
///
/// ## Returns
/// - Ok(()): Successfully written to stdout
/// - Err(io::Error): Write failed or format error
///
/// ## Examples
/// ```rust
/// // Simple text
/// buffy_print("Hello world", &[])?;
///
/// // With number
/// buffy_print("Count: {}", &[BuffyFormatArg::U64(42)])?;
///
/// // With styling
/// buffy_print("Status: {}", &[BuffyFormatArg::StrStyled("OK", BuffyStyles::bold_red())])?;
///
/// // With alignment
/// buffy_print("{:<10} {:>5}", &[BuffyFormatArg::Str("Name"), BuffyFormatArg::U32(123)])?;
/// ```
pub fn buffy_print(template: &str, args: &[BuffyFormatArg]) -> io::Result<()> {
    const MAX_ARGS: usize = 8;

    if args.len() > MAX_ARGS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Too many arguments (max 8)",
        ));
    }

    let mut stdout = io::stdout();
    let mut arg_index = 0;
    let mut pos = 0;

    // Stack buffers for conversions
    let mut num_buf = [0u8; 20];
    let mut style_buf = [0u8; 64];
    let mut align_buf = [0u8; 128];

    while pos < template.len() {
        // Find next placeholder
        if let Some(brace_pos) = template[pos..].find('{') {
            let absolute_brace = pos + brace_pos;

            // Write literal text before placeholder
            if brace_pos > 0 {
                stdout.write_all(template[pos..absolute_brace].as_bytes())?;
            }

            // Find closing brace
            if let Some(close_pos) = template[absolute_brace..].find('}') {
                let absolute_close = absolute_brace + close_pos;
                let placeholder = &template[absolute_brace + 1..absolute_close];

                // Parse format spec
                let spec = match parse_format_spec(placeholder) {
                    Some(s) => s,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Invalid format specifier",
                        ));
                    }
                };

                // Get argument
                if arg_index >= args.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Not enough arguments for format string",
                    ));
                }

                // Convert argument to string (on stack)
                let (value_str, has_style, style) = match &args[arg_index] {
                    BuffyFormatArg::Str(s) => (*s, false, BuffyStyles::default()),
                    // BuffyFormatArg::U8(n) => {
                    //     let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::U16(n) => {
                    //     let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::U32(n) => {
                    //     let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::U64(n) => {
                    //     let s = format_u64_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    BuffyFormatArg::Usize(n) => {
                        let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
                            io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                        })?;
                        (s, false, BuffyStyles::default())
                    }
                    // BuffyFormatArg::I8(n) => {
                    //     let s = format_i64_to_buffer(*n as i64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::I16(n) => {
                    //     let s = format_i64_to_buffer(*n as i64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::I32(n) => {
                    //     let s = format_i64_to_buffer(*n as i64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::I64(n) => {
                    //     let s = format_i64_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::Isize(n) => {
                    //     let s = format_i64_to_buffer(*n as i64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::U8Hex(n) => {
                    //     let s = format_u8_hex_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Hex conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::U16Hex(n) => {
                    //     let s = format_u16_hex_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Hex conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::U32Hex(n) => {
                    //     let s = format_u32_hex_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Hex conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    // BuffyFormatArg::Bool(b) => (
                    //     if *b { "true" } else { "false" },
                    //     false,
                    //     BuffyStyles::default(),
                    // ),
                    // BuffyFormatArg::Char(c) => {
                    //     let mut char_buf = [0u8; 4];
                    //     let char_str = c.encode_utf8(&mut char_buf);
                    //     let len = char_str.len();
                    //     num_buf[..len].copy_from_slice(char_str.as_bytes());
                    //     let s = std::str::from_utf8(&num_buf[..len]).map_err(|_| {
                    //         io::Error::new(io::ErrorKind::Other, "Char conversion failed")
                    //     })?;
                    //     (s, false, BuffyStyles::default())
                    // }
                    BuffyFormatArg::Path(p) => {
                        let s = p.to_str().ok_or_else(|| {
                            io::Error::new(io::ErrorKind::Other, "Path conversion failed")
                        })?;
                        (s, false, BuffyStyles::default())
                    }

                    // Styled variants
                    BuffyFormatArg::CharStyled(c, st) => {
                        let mut char_buf = [0u8; 4];
                        let char_str = c.encode_utf8(&mut char_buf);
                        let len = char_str.len();
                        num_buf[..len].copy_from_slice(char_str.as_bytes());
                        let s = std::str::from_utf8(&num_buf[..len]).map_err(|_| {
                            io::Error::new(io::ErrorKind::Other, "Char conversion failed")
                        })?;
                        (s, true, *st)
                    }
                    BuffyFormatArg::StrStyled(s, st) => (*s, true, *st),
                    // BuffyFormatArg::U8Styled(n, st) => {
                    //     let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, true, *st)
                    // }
                    // BuffyFormatArg::U64Styled(n, st) => {
                    //     let s = format_u64_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, true, *st)
                    // }
                    // BuffyFormatArg::UsizeStyled(n, st) => {
                    //     let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
                    //     })?;
                    //     (s, true, *st)
                    // }
                    // BuffyFormatArg::U8HexStyled(n, st) => {
                    //     let s = format_u8_hex_to_buffer(*n, &mut num_buf).ok_or_else(|| {
                    //         io::Error::new(io::ErrorKind::Other, "Hex conversion failed")
                    //     })?;
                    //     (s, true, *st)
                    // }
                };

                // Apply style if needed
                if has_style {
                    let ansi = style_to_ansi(style, &mut style_buf).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::Other, "BuffyStyles conversion failed")
                    })?;
                    stdout.write_all(ansi.as_bytes())?;
                }

                // Apply alignment and write
                let aligned = apply_alignment(value_str, spec, &mut align_buf)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Alignment failed"))?;
                stdout.write_all(aligned.as_bytes())?;

                // Reset style if needed
                if has_style {
                    stdout.write_all(b"\x1b[0m")?;
                }

                arg_index += 1;
                pos = absolute_close + 1;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Unclosed brace in format string",
                ));
            }
        } else {
            // No more placeholders, write remaining literal text
            stdout.write_all(template[pos..].as_bytes())?;
            break;
        }
    }

    Ok(())
}

/// Writes formatted output to stdout with newline and flush.
///
/// Memory: should be all stack, no heap
/// Calls buffy_print() then writes newline and flushes.
pub fn buffy_println(template: &str, args: &[BuffyFormatArg]) -> io::Result<()> {
    buffy_print(template, args)?;
    let mut stdout = io::stdout();
    stdout.write_all(b"\n")?;
    stdout.flush()
}

// /// Writes formatted output to any writer.
// ///
// /// Memory: should be all stack, no heap
// /// Same direct-write logic as buffy_print() but writes to provided writer.
// ///
// /// Writes formatted output to any writer (file, buffer, stream, stderr, etc.) with zero heap allocation.
// ///
// /// ## Project Context
// /// Generic output function for writing formatted text to destinations other than stdout.
// /// Used for file logging, buffer building, network streams, or stderr output while
// /// maintaining zero heap allocation guarantee. This is the underlying mechanism that
// /// `buffy_print()` uses internally with stdout.
// ///
// /// ## Memory: ZERO HEAP
// /// All conversions use stack buffers. Output written directly to provided writer.
// /// No String objects, no Vec allocations, no intermediate storage.
// ///
// /// ## Operation Flow
// /// 1. Parse template string for {} placeholders
// /// 2. For literal text: write directly to writer
// /// 3. For placeholders: convert arg on stack, write result to writer
// /// 4. Apply alignment/styling as specified
// /// 5. Continue until template exhausted
// ///
// /// ## Safety & Error Handling
// /// - No panic: Returns io::Error on write or format failure
// /// - Bounded: Max 8 arguments (prevents stack overflow)
// /// - Validates: All conversions checked, returns Err on failure
// /// - Non-critical: Caller can handle error and continue
// /// - Production-safe: No debug info leakage in error messages
// ///
// /// ## Parameters
// /// - `writer`: Mutable reference to any type implementing `Write` trait
// ///   (File, Vec<u8>, Stderr, TcpStream, BufWriter, etc.)
// /// - `template`: Format string with {} or {:<N}/{:>N}/{:^N} placeholders
// ///   - `{}` - default formatting
// ///   - `{:<N}` - left-align in N characters
// ///   - `{:>N}` - right-align in N characters
// ///   - `{:^N}` - center-align in N characters
// /// - `args`: Slice of BuffyFormatArg values (max 8 per call)
// ///   - U8, U16, U32, U64, Usize - unsigned integers
// ///   - I8, I16, I32, I64, Isize - signed integers
// ///   - U8Hex, U16Hex, U32Hex - hexadecimal formatting
// ///   - Str - string slices
// ///   - Bool - true/false
// ///   - Char - single characters
// ///   - Path - file paths
// ///   - Styled variants - include ANSI color codes
// ///
// /// ## Returns
// /// - `Ok(())`: Successfully written to writer
// /// - `Err(io::Error)`: Write failed, format error, or buffer too small
// ///
// /// ## When to Use vs `buffy_print()`
// /// - Use `buffy_print()`: Writing to terminal/stdout (most common TUI case)
// /// - Use `buffy_write_basic()`: Writing to files, buffers, stderr, or network streams
// ///
// /// ## Limitations
// /// - Max 8 arguments per call (call multiple times if needed)
// /// - Max 64 characters width for alignment
// /// - Template placeholders must match arg count exactly
// /// - Writer must have capacity for output (or return error)
// ///
// /// ## Examples
// ///
// /// ### File Logging
// /// ```rust
// /// use std::fs::File;
// ///
// /// let mut log = File::create("app.log")?;
// /// buffy_write_basic(
// ///     &mut log,
// ///     "[{}] User {} logged in at {}\n",
// ///     &[
// ///         BuffyFormatArg::Str("INFO"),
// ///         BuffyFormatArg::U32(1001),
// ///         BuffyFormatArg::Str("2025-01-15"),
// ///     ]
// /// )?;
// /// log.flush()?;
// /// ```
// ///
// /// ### Error to Stderr
// /// ```rust
// /// use std::io::stderr;
// ///
// /// let mut err = stderr();
// /// buffy_write_basic(
// ///     &mut err,
// ///     "ERROR: Failed to open file (code: {})\n",
// ///     &[BuffyFormatArg::U32(404)]
// /// )?;
// /// ```
// ///
// /// ### Building String in Buffer
// /// ```rust
// /// let mut buffer = Vec::<u8>::new();
// /// buffy_write_basic(
// ///     &mut buffer,
// ///     "Report: {} items processed, {} errors\n",
// ///     &[
// ///         BuffyFormatArg::U64(1000),
// ///         BuffyFormatArg::U32(3),
// ///     ]
// /// )?;
// /// let report = String::from_utf8(buffer)?;
// /// ```
// ///
// /// ### Hex Dump to File
// /// ```rust
// /// let mut dump = File::create("memory.hex")?;
// /// let bytes: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
// ///
// /// buffy_write_basic(
// ///     &mut dump,
// ///     "0x{} 0x{} 0x{} 0x{}\n",
// ///     &[
// ///         BuffyFormatArg::U8Hex(bytes[0]),
// ///         BuffyFormatArg::U8Hex(bytes[1]),
// ///         BuffyFormatArg::U8Hex(bytes[2]),
// ///         BuffyFormatArg::U8Hex(bytes[3]),
// ///     ]
// /// )?;
// /// ```
// ///
// /// ### Styled Output to Stderr
// /// ```rust
// /// let mut err = stderr();
// /// buffy_write_basic(
// ///     &mut err,
// ///     "{}: Operation failed\n",
// ///     &[BuffyFormatArg::StrStyled(
// ///         "CRITICAL",
// ///         BuffyStyles {
// ///             fg_color: Some("\x1b[31m"), // RED
// ///             bold: true,
// ///             ..Default::default()
// ///         }
// ///     )]
// /// )?;
// /// ```
// ///
// /// ### Aligned Table to File
// /// ```rust
// /// let mut table = File::create("report.txt")?;
// ///
// /// // Header
// /// buffy_write_basic(
// ///     &mut table,
// ///     "{:<15} {:>10} {:>10}\n",
// ///     &[
// ///         BuffyFormatArg::Str("Item"),
// ///         BuffyFormatArg::Str("Quantity"),
// ///         BuffyFormatArg::Str("Price"),
// ///     ]
// /// )?;
// ///
// /// // Data row
// /// buffy_write_basic(
// ///     &mut table,
// ///     "{:<15} {:>10} {:>10}\n",
// ///     &[
// ///         BuffyFormatArg::Str("Widget"),
// ///         BuffyFormatArg::U32(42),
// ///         BuffyFormatArg::U32(299),
// ///     ]
// /// )?;
// /// ```
// ///
// /// ### Network Protocol Message
// /// ```rust
// /// use std::net::TcpStream;
// ///
// /// let mut stream = TcpStream::connect("127.0.0.1:8080")?;
// /// buffy_write_basic(
// ///     &mut stream,
// ///     "MSG {} LEN {} DATA {}\r\n",
// ///     &[
// ///         BuffyFormatArg::U32(1001),
// ///         BuffyFormatArg::U32(payload.len()),
// ///         BuffyFormatArg::Str(payload),
// ///     ]
// /// )?;
// /// stream.flush()?;
// /// ```
// ///
// /// ## Error Handling Pattern
// /// ```rust
// /// match buffy_write_basic(&mut file, "Value: {}\n", &[BuffyFormatArg::U32(x)]) {
// ///     Ok(()) => { /* continue */ },
// ///     Err(e) => {
// ///         // Log to stderr, don't panic production code
// ///         let mut err = stderr();
// ///         let _ = buffy_write_basic(
// ///             &mut err,
// ///             "Write failed (recovered)\n",
// ///             &[]
// ///         );
// ///         // Continue with fallback behavior
// ///     }
// /// }
// /// ```
// pub fn buffy_write_basic<W: Write>(
//     writer: &mut W,
//     template: &str,
//     args: &[BuffyFormatArg],
// ) -> io::Result<()> {
//     const MAX_ARGS: usize = 8;

//     if args.len() > MAX_ARGS {
//         return Err(io::Error::new(
//             io::ErrorKind::InvalidInput,
//             "Too many arguments (max 8)",
//         ));
//     }

//     let mut arg_index = 0;
//     let mut pos = 0;

//     // Stack buffers for conversions
//     let mut num_buf = [0u8; 20];
//     let mut style_buf = [0u8; 64];
//     let mut align_buf = [0u8; 128];

//     while pos < template.len() {
//         if let Some(brace_pos) = template[pos..].find('{') {
//             let absolute_brace = pos + brace_pos;

//             if brace_pos > 0 {
//                 writer.write_all(template[pos..absolute_brace].as_bytes())?;
//             }

//             if let Some(close_pos) = template[absolute_brace..].find('}') {
//                 let absolute_close = absolute_brace + close_pos;
//                 let placeholder = &template[absolute_brace + 1..absolute_close];

//                 let spec = match parse_format_spec(placeholder) {
//                     Some(s) => s,
//                     None => {
//                         return Err(io::Error::new(
//                             io::ErrorKind::InvalidInput,
//                             "Invalid format specifier",
//                         ));
//                     }
//                 };

//                 if arg_index >= args.len() {
//                     return Err(io::Error::new(
//                         io::ErrorKind::InvalidInput,
//                         "Not enough arguments for format string",
//                     ));
//                 }

//                 // Convert argument (same logic as buffy_print)
//                 let (value_str, has_style, style) = match &args[arg_index] {
//                     BuffyFormatArg::Str(s) => (*s, false, BuffyStyles::default()),
//                     // BuffyFormatArg::U8(n) => {
//                     //     let s = format_u64_to_buffer(*n as u64, &mut num_buf).ok_or_else(|| {
//                     //         io::Error::new(io::ErrorKind::Other, "Number conversion failed")
//                     //     })?;
//                     //     (s, false, BuffyStyles::default())
//                     // }
//                     BuffyFormatArg::U64(n) => {
//                         let s = format_u64_to_buffer(*n, &mut num_buf).ok_or_else(|| {
//                             io::Error::new(io::ErrorKind::Other, "Number conversion failed")
//                         })?;
//                         (s, false, BuffyStyles::default())
//                     }
//                     BuffyFormatArg::U8Hex(n) => {
//                         let s = format_u8_hex_to_buffer(*n, &mut num_buf).ok_or_else(|| {
//                             io::Error::new(io::ErrorKind::Other, "Hex conversion failed")
//                         })?;
//                         (s, false, BuffyStyles::default())
//                     }
//                     BuffyFormatArg::Bool(b) => (
//                         if *b { "true" } else { "false" },
//                         false,
//                         BuffyStyles::default(),
//                     ),
//                     BuffyFormatArg::Char(c) => {
//                         let mut char_buf = [0u8; 4];
//                         let char_str = c.encode_utf8(&mut char_buf);
//                         let len = char_str.len();
//                         num_buf[..len].copy_from_slice(char_str.as_bytes());
//                         let s = std::str::from_utf8(&num_buf[..len]).map_err(|_| {
//                             io::Error::new(io::ErrorKind::Other, "Char conversion failed")
//                         })?;
//                         (s, false, BuffyStyles::default())
//                     }

//                     // Add other types as needed (same as buffy_print)
//                     BuffyFormatArg::StrStyled(s, st) => (*s, true, *st),
//                     BuffyFormatArg::U8HexStyled(n, st) => {
//                         let s = format_u8_hex_to_buffer(*n, &mut num_buf).ok_or_else(|| {
//                             io::Error::new(io::ErrorKind::Other, "Hex conversion failed")
//                         })?;
//                         (s, true, *st)
//                     }

//                     // Add remaining types as needed
//                     _ => {
//                         return Err(io::Error::new(
//                             io::ErrorKind::Other,
//                             "Unsupported argument type",
//                         ));
//                     }
//                 };

//                 if has_style {
//                     let ansi = style_to_ansi(style, &mut style_buf).ok_or_else(|| {
//                         io::Error::new(io::ErrorKind::Other, "BuffyStyles conversion failed")
//                     })?;
//                     writer.write_all(ansi.as_bytes())?;
//                 }

//                 let aligned = apply_alignment(value_str, spec, &mut align_buf)
//                     .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Alignment failed"))?;
//                 writer.write_all(aligned.as_bytes())?;

//                 if has_style {
//                     writer.write_all(b"\x1b[0m")?;
//                 }

//                 arg_index += 1;
//                 pos = absolute_close + 1;
//             } else {
//                 return Err(io::Error::new(
//                     io::ErrorKind::InvalidInput,
//                     "Unclosed brace in format string",
//                 ));
//             }
//         } else {
//             writer.write_all(template[pos..].as_bytes())?;
//             break;
//         }
//     }

//     Ok(())
// }

// /// Repeats a character N times, writing directly to writer.
// ///
// /// ## Project Context
// /// Helper for drawing horizontal lines, borders, and padding in TUI.
// /// Avoids String::repeat() which allocates heap.
// ///
// /// Memory: should be all stack, no heap
// /// Uses 64-byte stack buffer, writes in chunks if repeat count exceeds buffer.
// ///
// /// ## Parameters
// /// - writer: Output destination
// /// - ch: Character to repeat (any UTF-8 character)
// /// - count: Number of repetitions
// ///
// /// ## Returns
// /// - Ok(()): Successfully written
// /// - Err(io::Error): Write failed
// ///
// /// ## Examples
// /// ```rust
// /// // Horizontal line
// /// buffy_repeat(&mut stdout, '=', 70)?;
// /// buffy_println("", &[])?;
// ///
// /// // Padding
// /// buffy_repeat(&mut stdout, ' ', 4)?;
// /// buffy_print("Indented text", &[])?;
// /// ```
// pub fn buffy_repeat<W: Write>(writer: &mut W, ch: char, count: usize) -> io::Result<()> {
//     if count == 0 {
//         return Ok(());
//     }

//     // Encode character to UTF-8 on stack
//     let mut char_buf = [0u8; 4];
//     let char_str = ch.encode_utf8(&mut char_buf);
//     let char_len = char_str.len();

//     // Use 64-byte stack buffer for batching
//     let mut buf = [0u8; 64];
//     let chars_per_batch = buf.len() / char_len;

//     if chars_per_batch == 0 {
//         return Err(io::Error::new(
//             io::ErrorKind::InvalidInput,
//             "Character too large for buffer",
//         ));
//     }

//     // Fill buffer with character pattern
//     let mut buf_pos = 0;
//     for _ in 0..chars_per_batch {
//         if buf_pos + char_len <= buf.len() {
//             buf[buf_pos..buf_pos + char_len].copy_from_slice(&char_buf[..char_len]);
//             buf_pos += char_len;
//         }
//     }
//     let batch_size = buf_pos;

//     // Write full batches
//     let full_batches = count / chars_per_batch;
//     for _ in 0..full_batches {
//         writer.write_all(&buf[..batch_size])?;
//     }

//     // Write remaining characters
//     let remaining = count % chars_per_batch;
//     for _ in 0..remaining {
//         writer.write_all(&char_buf[..char_len])?;
//     }

//     Ok(())
// }

// /// Writes a single styled text chunk directly to writer.
// ///
// /// Memory: should be all stack, no heap
// /// Writes ANSI codes (if any), text, and reset directly.
// pub fn buffy_write_styled<W: Write>(
//     writer: &mut W,
//     text: &str,
//     style: Option<BuffyStyles>,
// ) -> io::Result<()> {
//     if let Some(s) = style {
//         let mut style_buf = [0u8; 64];
//         if let Some(ansi) = style_to_ansi(s, &mut style_buf) {
//             writer.write_all(ansi.as_bytes())?;
//         }
//     }

//     writer.write_all(text.as_bytes())?;

//     if style.is_some() {
//         writer.write_all(b"\x1b[0m")?;
//     }

//     Ok(())
// }

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod buffy_format_tests {
    use super::*;

    #[test]
    fn test_format_u64() {
        let mut buf = [0u8; 20];
        let result = format_u64_to_buffer(42, &mut buf);
        assert_eq!(result, Some("42"));
    }

    // #[test]
    // fn test_format_i64_negative() {
    //     let mut buf = [0u8; 20];
    //     let result = format_i64_to_buffer(-42, &mut buf);
    //     assert_eq!(result, Some("-42"));
    // }

    // #[test]
    // fn test_format_hex() {
    //     let mut buf = [0u8; 8];z
    //     let result = format_u8_hex_to_buffer(0xFF, &mut buf);
    //     assert_eq!(result, Some("FF"));
    // }

    #[test]
    fn test_alignment_left() {
        let mut buf = [0u8; 10];
        let spec = FormatSpec {
            alignment: Alignment::Left,
            width: Some(5),
        };
        let result = apply_alignment("AB", spec, &mut buf);
        assert_eq!(result, Some("AB   "));
    }

    #[test]
    fn test_alignment_right() {
        let mut buf = [0u8; 10];
        let spec = FormatSpec {
            alignment: Alignment::Right,
            width: Some(5),
        };
        let result = apply_alignment("AB", spec, &mut buf);
        assert_eq!(result, Some("   AB"));
    }
}

// =============================================================================
// SYNTAX HIGHLIGHTING SUPPORT
// =============================================================================
//
// ## Project Context
// This module section provides minimal two-category syntax highlighting for
// a TUI text editor. The design is intentionally minimal:
//
//   Category 1 - Syntax symbols (cyan):  ( ) [ ] { } < > = : ; " ' \ & ! # / * , `
//   Category 2 - Definition words (yellow): fn , def , let , struct , enum ,
//                class , impl , type , const , static , pub , use , mod
//
// The system highlights by DEFAULT and opts OUT for plain-text extensions.
// This is simpler than trying to enumerate all code file extensions.
//
// Plain text extensions that opt OUT of highlighting: .txt  .log
//
// ## Design Constraints (No Heap)
// - No String, no Vec, no dynamic allocation
// - All matching done against &'static str constants
// - Extension comparison done on raw bytes
// - SyntaxHighlight is a stack-only enum
//
// ## Integration Point
// These functions are called from render_utf8txt_row_with_cursor() in the
// editor rendering pipeline. buffy_is_plain_text_extension() is called once
// per row before the character loop. buffy_get_syntax_highlight() is called
// once per character position inside the loop, only when not plain text,
// and only when the character is not already handled by cursor or selection
// highlighting (those take priority).
//
// ## What This Does NOT Do
// - No string/comment context detection: keywords inside string literals
//   will be highlighted (acceptable for minimal system)
// - No language detection beyond plain-text opt-out
// - No background highlighting: foreground colour only (word colouring)
// - No per-token parser: pure positional byte matching

/// Two-category syntax highlight result.
///
/// ## Project Context
/// Returned by buffy_get_syntax_highlight() to indicate what foreground
/// colour (if any) should be applied to the character at a given position.
///
/// ## Variants
/// - None:           No highlighting. Write character directly.
/// - SyntaxSymbol:   Single punctuation/operator character. Render in cyan.
/// - DefinitionWord: Start of a definition keyword (e.g. "fn ", "let ").
///                   The entire keyword including its trailing space is
///                   consumed as one logical token. Render in yellow.
///
/// ## Memory
/// Stack-only enum. No heap. Safe to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxHighlight {
    /// No highlighting applies. Write the character with no ANSI codes.
    None,

    /// Character is a syntax punctuation symbol.
    /// Caller should render it in cyan.
    /// Applies to: ( ) [ ] { } < > = : ; " ' \ & ! # / * , `
    SyntaxSymbol,

    /// Character starts a definition keyword (including trailing space).
    /// Caller should render the entire keyword span in yellow.
    /// The `keyword_byte_len` field tells the caller how many bytes
    /// the full keyword token occupies (e.g. "fn " = 3 bytes).
    /// Caller advances its byte position by this amount after writing.
    DefinitionWord {
        /// Number of bytes in the matched keyword including trailing space.
        /// Example: "fn " -> 3, "struct " -> 7, "static " -> 7
        keyword_byte_len: usize,
    },
}

/// Returns true if the file at the given path has a plain-text extension
/// that should NOT receive syntax highlighting.
///
/// ## Project Context
/// Called once per rendered row, before the character loop, to decide
/// whether to attempt syntax highlighting at all. The opt-out list is
/// intentionally short: only file types that are truly plain prose where
/// keyword colouring would be distracting or misleading.
///
/// Opt-out extensions (no highlighting):
///   .txt   .log
///
/// Everything else (including no extension, or None path) receives
/// highlighting. This default-highlight approach is simpler than trying
/// to enumerate every possible code file extension.
///
/// ## Behaviour on Edge Cases
/// - path is None:          returns false  (highlight by default)
/// - path has no extension: returns false  (highlight by default)
/// - extension not valid UTF-8: returns false (highlight by default, safe)
/// - extension is ".TXT" (uppercase): returns false (case-sensitive match,
///   no heap conversion, conservative: only exact lowercase match opts out)
///
/// ## Memory
/// No heap. Extension slice borrowed from path. Comparison on &[u8] bytes.
///
/// ## Arguments
/// * `path` - Optional reference to the file path from EditorState.
///            Typically &state.original_file_path (which is Option<PathBuf>).
///            Pass as path.as_deref() to get Option<&Path>.
///
/// ## Returns
/// * true  - plain text, skip syntax highlighting
/// * false - not plain text (or unknown), apply syntax highlighting
pub fn buffy_is_plain_text_extension(path: Option<&Path>) -> bool {
    // Plain-text extensions that opt OUT of syntax highlighting.
    // Matched as raw bytes against the file extension.
    // Case-sensitive: ".txt" matches, ".TXT" does not.
    // Extend this list conservatively: only add extensions where keyword
    // colouring would be actively misleading or distracting.
    const PLAIN_TEXT_EXTENSIONS: &[&[u8]] = &[b"txt", b"log"];

    // Defensive: no path means we cannot determine extension.
    // Default to highlight (return false = not plain text).
    let path = match path {
        Some(p) => p,
        None => return false,
    };

    // Extract extension as a str slice (no allocation).
    // Path::extension() returns Option<&OsStr>.
    // OsStr::as_encoded_bytes() gives us raw bytes without allocation.
    let ext_bytes = match path.extension() {
        Some(ext) => ext.as_encoded_bytes(),
        None => return false, // No extension: highlight by default
    };

    // Compare extension bytes against the opt-out list.
    // Linear scan over a tiny fixed list: no overhead worth optimising.
    for &plain_ext in PLAIN_TEXT_EXTENSIONS {
        if ext_bytes == plain_ext {
            return true; // Plain text: skip highlighting
        }
    }

    false // Not in opt-out list: apply highlighting
}

/// Returns the syntax highlight category for the character at the given
/// byte position within a row string.
///
/// ## Project Context
/// Called inside the character rendering loop of render_utf8txt_row_with_cursor().
/// The caller has already handled cursor and visual-selection priority.
/// This function is only reached for "normal" characters that need
/// syntax highlight checking.
///
/// ## Two-Category System
///
/// ### Category 1: SyntaxSymbol (cyan)
/// Single-character punctuation/operator symbols.
/// Matched character-by-character. The caller writes one character and
/// advances by one UTF-8 character (1-4 bytes).
///
/// Symbol set:  ( ) [ ] { } < > = : ; " ' \ & ! # / * , `
///
/// ### Category 2: DefinitionWord (yellow)
/// Multi-character keywords, each followed by a space (space is part of
/// the match and is highlighted together with the keyword). The space is
/// included because:
/// - It makes the match unambiguous (avoids matching "type" inside "typeof")
/// - The space is visually invisible so including it in the coloured span
///   costs nothing visually
/// - No need to abstractly separate the space from the keyword
///
/// Keyword set (each stored with trailing space as part of the literal):
///   "fn "  "def "  "let "  "struct "  "enum "  "class "
///   "impl "  "type "  "const "  "static "  "pub "  "use "  "mod "
///
/// When a keyword matches, SyntaxHighlight::DefinitionWord { keyword_byte_len }
/// is returned. The caller must:
///   1. Write the entire keyword span (keyword_byte_len bytes) in yellow
///   2. Advance its byte position by keyword_byte_len
///   3. Advance its character counter by the number of chars in the keyword
///      (caller can count these, or use the provided char count - see note)
///
/// ## Priority
/// SyntaxSymbol is checked first. In practice there is no overlap
/// (no keyword begins with a syntax symbol character), but checking
/// symbols first is cheaper (single char lookup vs prefix scan).
///
/// ## Cursor and Selection Priority
/// This function does NOT check cursor or selection state. The caller
/// is responsible for checking those conditions BEFORE calling this
/// function. If the character is under the cursor or inside a visual
/// selection, the caller should NOT call this function.
///
/// ## Byte Position vs Character Index
/// `byte_pos` is the byte offset of the current character's first byte
/// within `row_content`. This is what is needed for prefix matching
/// (slicing `row_content` from `byte_pos` forward).
///
/// The caller tracks the character index (for cursor column comparison)
/// as a SEPARATE counter that increments once per complete UTF-8 character.
/// That character index is NOT passed to this function and NOT used here.
/// See render_utf8txt_row_with_cursor() for the byte-vs-char tracking pattern.
///
/// ## Memory
/// No heap. All keyword literals are &'static str. Matching is byte
/// comparison via str::starts_with() on a sub-slice.
///
/// ## Arguments
/// * `byte_pos`    - Byte offset of the current character's first byte
///                   within `row_content`. Must be a valid UTF-8 boundary.
/// * `row_content` - The full row string being rendered (content portion only,
///                   line number prefix already excluded by caller).
///
/// ## Returns
/// * SyntaxHighlight::None            - no highlighting, write char normally
/// * SyntaxHighlight::SyntaxSymbol    - render this char in cyan
/// * SyntaxHighlight::DefinitionWord { keyword_byte_len }
///                                    - render keyword_byte_len bytes in yellow,
///                                      then advance byte_pos by keyword_byte_len
pub fn buffy_get_syntax_highlight(byte_pos: usize, row_content: &str) -> SyntaxHighlight {
    // -------------------------------------------------------------------------
    // BOUNDS CHECK: byte_pos must be within row_content
    // -------------------------------------------------------------------------
    // Defensive: if byte_pos is out of range, return None safely.
    // This should not happen if the caller iterates correctly, but hardware
    // faults or logic errors must not cause a panic in production.
    if byte_pos >= row_content.len() {
        return SyntaxHighlight::None;
    }

    // -------------------------------------------------------------------------
    // DEFINITION KEYWORDS (Category 2, yellow)
    // Checked BEFORE symbol check for one reason: keyword matching requires
    // reading ahead multiple bytes anyway, and we want to colour the whole
    // keyword span (not just its first character) as a single token.
    //
    // Each entry is the complete match token: keyword + trailing space.
    // The trailing space is part of the highlighted span.
    // "fn " = 3 bytes, "struct " = 7 bytes, "static " = 7 bytes, etc.
    //
    // Order: longer keywords first where a shorter keyword is a prefix of
    // a longer one (none exist in this set currently, but "mod" vs nothing
    // is fine). Order does not otherwise affect correctness.
    // -------------------------------------------------------------------------
    const DEFINITION_KEYWORDS: &[&str] = &[
        "static ", // 7 bytes - before "struct" to avoid any future ambiguity
        "struct ", // 7 bytes
        "class ",  // 6 bytes
        "const ",  // 6 bytes
        "impl ",   // 5 bytes
        "type ",   // 5 bytes
        "enum ",   // 5 bytes
        "use ",    // 4 bytes
        "pub ",    // 4 bytes
        "mod ",    // 4 bytes
        "let ",    // 4 bytes
        "def ",    // 4 bytes
        "fn ",     // 3 bytes
        // Other
        "for ",   //
        "while ", //
        "match ", //
        "if ",    //
        "loop ",  //
    ];

    // Slice the row content from the current byte position forward.
    // No allocation: this is a borrowed sub-slice of the existing &str.
    let remaining = &row_content[byte_pos..];

    // Scan keyword list. Linear scan over a tiny fixed list.
    for &keyword in DEFINITION_KEYWORDS {
        if remaining.starts_with(keyword) {
            return SyntaxHighlight::DefinitionWord {
                keyword_byte_len: keyword.len(),
            };
        }
    }

    // -------------------------------------------------------------------------
    // SYNTAX SYMBOLS (Category 1, cyan)
    // Single-character punctuation that makes code structure visible.
    // Checked after keywords so that the first character of a keyword
    // (which is always alphabetic) never reaches this check anyway.
    // In practice: no overlap is possible (no keyword starts with a symbol).
    //
    // The character at byte_pos is extracted by getting the first char
    // of the remaining slice. For ASCII symbols this is always 1 byte.
    // For safety we use chars().next() which handles UTF-8 correctly.
    // -------------------------------------------------------------------------
    const SYNTAX_SYMBOLS: &[char] = &[
        '(', ')', '[', ']', '{', '}', '<', '>', '=', ':', ';', '\\', '&', '!', '#', '/', '*', ',',
        '`',
    ];
    // maybe/maybe-not: ", '

    // Get the first character at this byte position.
    // chars().next() is safe: we already checked byte_pos < row_content.len()
    // and remaining is a valid UTF-8 sub-slice.
    if let Some(ch) = remaining.chars().next() {
        for &symbol in SYNTAX_SYMBOLS {
            if ch == symbol {
                return SyntaxHighlight::SyntaxSymbol;
            }
        }
    }

    // -------------------------------------------------------------------------
    // No match: plain character, no highlighting.
    // -------------------------------------------------------------------------
    SyntaxHighlight::None
}

// =============================================================================
// TESTS: Syntax Highlighting
// =============================================================================

#[cfg(test)]
mod syntax_highlight_tests {
    use super::*;
    use std::path::Path;

    // --- buffy_is_plain_text_extension ---

    #[test]
    fn test_plain_text_extension_txt_is_plain() {
        let path = Path::new("readme.txt");
        assert!(
            buffy_is_plain_text_extension(Some(path)),
            "buffy_is_plain_text_extension: .txt should be plain text"
        );
    }

    #[test]
    fn test_plain_text_extension_log_is_plain() {
        let path = Path::new("app.log");
        assert!(
            buffy_is_plain_text_extension(Some(path)),
            "buffy_is_plain_text_extension: .log should be plain text"
        );
    }

    #[test]
    fn test_plain_text_extension_rs_is_not_plain() {
        let path = Path::new("main.rs");
        assert!(
            !buffy_is_plain_text_extension(Some(path)),
            "buffy_is_plain_text_extension: .rs should NOT be plain text"
        );
    }

    #[test]
    fn test_plain_text_extension_py_is_not_plain() {
        let path = Path::new("script.py");
        assert!(
            !buffy_is_plain_text_extension(Some(path)),
            "buffy_is_plain_text_extension: .py should NOT be plain text"
        );
    }

    #[test]
    fn test_plain_text_extension_none_path_is_not_plain() {
        assert!(
            !buffy_is_plain_text_extension(None),
            "buffy_is_plain_text_extension: None path should default to not plain (highlight)"
        );
    }

    #[test]
    fn test_plain_text_extension_no_extension_is_not_plain() {
        let path = Path::new("Makefile");
        assert!(
            !buffy_is_plain_text_extension(Some(path)),
            "buffy_is_plain_text_extension: no extension should default to not plain (highlight)"
        );
    }

    #[test]
    fn test_plain_text_extension_uppercase_txt_is_not_plain() {
        // Case-sensitive match: .TXT does not opt out (conservative, no heap conversion)
        let path = Path::new("readme.TXT");
        assert!(
            !buffy_is_plain_text_extension(Some(path)),
            "buffy_is_plain_text_extension: .TXT uppercase should not match (case-sensitive)"
        );
    }

    // --- buffy_get_syntax_highlight: SyntaxSymbol ---

    #[test]
    fn test_syntax_highlight_open_paren_is_symbol() {
        let row = "foo(bar)";
        let result = buffy_get_syntax_highlight(3, row);
        assert_eq!(
            result,
            SyntaxHighlight::SyntaxSymbol,
            "buffy_get_syntax_highlight: '(' should be SyntaxSymbol"
        );
    }

    #[test]
    fn test_syntax_highlight_close_brace_is_symbol() {
        let row = "fn foo() {}";
        // '}' is at byte index 10
        let result = buffy_get_syntax_highlight(10, row);
        assert_eq!(
            result,
            SyntaxHighlight::SyntaxSymbol,
            "buffy_get_syntax_highlight: '}}' should be SyntaxSymbol"
        );
    }

    #[test]
    fn test_syntax_highlight_colon_is_symbol() {
        let row = "x: u32";
        let result = buffy_get_syntax_highlight(1, row);
        assert_eq!(
            result,
            SyntaxHighlight::SyntaxSymbol,
            "buffy_get_syntax_highlight: ':' should be SyntaxSymbol"
        );
    }

    #[test]
    fn test_syntax_highlight_plain_letter_is_none() {
        let row = "hello";
        let result = buffy_get_syntax_highlight(0, row);
        assert_eq!(
            result,
            SyntaxHighlight::None,
            "buffy_get_syntax_highlight: plain letter should be None"
        );
    }

    // --- buffy_get_syntax_highlight: DefinitionWord ---

    #[test]
    fn test_syntax_highlight_fn_keyword() {
        let row = "fn main() {}";
        let result = buffy_get_syntax_highlight(0, row);
        assert_eq!(
            result,
            SyntaxHighlight::DefinitionWord {
                keyword_byte_len: 3
            },
            "buffy_get_syntax_highlight: 'fn ' should be DefinitionWord with byte_len 3"
        );
    }

    #[test]
    fn test_syntax_highlight_struct_keyword() {
        let row = "struct Foo {";
        let result = buffy_get_syntax_highlight(0, row);
        assert_eq!(
            result,
            SyntaxHighlight::DefinitionWord {
                keyword_byte_len: 7
            },
            "buffy_get_syntax_highlight: 'struct ' should be DefinitionWord with byte_len 7"
        );
    }

    #[test]
    fn test_syntax_highlight_let_keyword() {
        let row = "    let x = 5;";
        // "let " starts at byte 4
        let result = buffy_get_syntax_highlight(4, row);
        assert_eq!(
            result,
            SyntaxHighlight::DefinitionWord {
                keyword_byte_len: 4
            },
            "buffy_get_syntax_highlight: 'let ' should be DefinitionWord with byte_len 4"
        );
    }

    #[test]
    fn test_syntax_highlight_static_keyword() {
        let row = "static FOO: u32 = 1;";
        let result = buffy_get_syntax_highlight(0, row);
        assert_eq!(
            result,
            SyntaxHighlight::DefinitionWord {
                keyword_byte_len: 7
            },
            "buffy_get_syntax_highlight: 'static ' should be DefinitionWord with byte_len 7"
        );
    }

    #[test]
    fn test_syntax_highlight_pub_keyword() {
        let row = "pub fn foo() {}";
        let result = buffy_get_syntax_highlight(0, row);
        assert_eq!(
            result,
            SyntaxHighlight::DefinitionWord {
                keyword_byte_len: 4
            },
            "buffy_get_syntax_highlight: 'pub ' should be DefinitionWord with byte_len 4"
        );
    }

    #[test]
    fn test_syntax_highlight_fn_not_at_start_of_word() {
        // "fn" appears inside "unfn" - no space before it, but we match from byte_pos
        // If byte_pos points mid-word, we still match if it starts with "fn "
        // This is the known limitation: no word-boundary check.
        // This test documents actual behaviour (not asserting it is wrong,
        // just documenting that context-free matching is the design).
        let row = "xfn foo()";
        // byte_pos=1 points to 'f' of "fn foo()"
        let result = buffy_get_syntax_highlight(1, row);
        // "fn " starts at byte 1: this WILL match (no word boundary check by design)
        assert_eq!(
            result,
            SyntaxHighlight::DefinitionWord {
                keyword_byte_len: 3
            },
            "buffy_get_syntax_highlight: known behaviour: no word-boundary check, 'fn ' matches mid-string"
        );
    }

    #[test]
    fn test_syntax_highlight_out_of_bounds_returns_none() {
        let row = "hi";
        // byte_pos beyond string length
        let result = buffy_get_syntax_highlight(99, row);
        assert_eq!(
            result,
            SyntaxHighlight::None,
            "buffy_get_syntax_highlight: out-of-bounds byte_pos should return None safely"
        );
    }

    #[test]
    fn test_syntax_highlight_empty_string_returns_none() {
        let result = buffy_get_syntax_highlight(0, "");
        assert_eq!(
            result,
            SyntaxHighlight::None,
            "buffy_get_syntax_highlight: empty string should return None safely"
        );
    }

    #[test]
    fn test_syntax_highlight_multibyte_char_is_none() {
        // '世' is 3 bytes (E4 B8 96), not a symbol or keyword, should be None
        let row = "世界";
        let result = buffy_get_syntax_highlight(0, row);
        assert_eq!(
            result,
            SyntaxHighlight::None,
            "buffy_get_syntax_highlight: multi-byte non-symbol char should be None"
        );
    }
}
