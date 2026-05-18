
## Part 2 Scope: Notation Parsing
2026.05.17

for reference:
```
struct ParsedMoveNotation {
    piece_kind: PieceKind,
    destination_file: u8,
    destination_rank: u8,
    is_capture: bool,
    disambiguation_source_file: Option<u8>,
    disambiguation_source_rank: Option<u8>,
    promotion_piece_kind: Option<PieceKind>,
    explicit_source_file: Option<u8>,         // for long algebraic
    explicit_source_rank: Option<u8>,         // for long algebraic
    is_castle_kingside: bool,
    is_castle_queenside: bool,
}
```

### ~Three Functions: (fo Notation Parsing)
1. pre_screen_and_normalize_notation_input
2. parse_move_notation
3. parse_non_move_player_command


1. **`pre_screen_and_normalize_notation_input(input: &[u8], output_buffer: &mut [u8; 9]) -> Option<u8>`**
A. remove all spaces & parentheses
B. .lower()
C. look for rejection-cases:
- reject if Empty input (e.g. only whitespace after removing spaces)
- length: reject if Over-9-character input after stripping
- reject if contains any non-allowed/permitted char
D. returns something (not rejected) or nothing (rejected or empty)


Maybe:
### Function 1: `pre_screen_and_normalize_notation_input`
```rust
pub fn pre_screen_and_normalize_notation_input(
    input: &[u8],
    output_buffer: &mut [u8; 9],
) -> Option<u8>
```
- Returns `Some(length)` on success, `None` on rejection.
- Steps (in order):
  1. Iterate `input` bytes. Skip ASCII space (`b' '`), tab (`b'\t'`), CR (`b'\r'`), LF (`b'\n'`), `b'('`, `b')'`.
  2. For each remaining byte: reject (return `None`) if it is non-ASCII (`>= 128`).
  3. Lowercase ASCII uppercase letters (`b'A'..=b'Z'` → add 32).
  4. Reject if the lowercased byte is not in the allowed set:
     - Digits: `0` `1` `2` `3` `4` `5` `6` `7` `8`
     - Symbols: `=` `-` `+` `#` `!` `?`
     - Letters: `a` `b` `c` `d` `e` `f` `g` `h` `i` `k` `n` `o` `p` `q` `r` `s` `w` `x`
       (Note: `i`, `s`, `w` are included only to permit `"resign"` and `"draw"`; `g` is already a file letter.)
  5. Write the byte into `output_buffer`. If the running length would exceed 9, reject.
  6. After the loop: if running length is 0, reject.
- Returns `Some(length_written)` otherwise.

or
### Function 1: `pre_screen_and_normalize_notation_input`

**Signature:**
```rust
pub fn pre_screen_and_normalize_notation_input(
    input: &[u8],
    output_buffer: &mut [u8; 9],
) -> Option<u8>
```

**Behavior (in order):**
1. Iterate input bytes.
2. Skip whitespace bytes (`b' '`, `b'\t'`, `b'\r'`, `b'\n'`) and parentheses (`b'('`, `b')'`).
3. Reject (return `None`) on any non-ASCII byte (`>= 128`).
4. Lowercase ASCII uppercase letters (`A`–`Z` → add 32).
5. Reject if the lowercased byte is not in the allowed set:
   - Digits: `0 1 2 3 4 5 6 7 8`
   - Symbols: `= - + # ! ?`
   - Letters: `a b c d e f g h i k n o p q r s w x`
6. Write each accepted byte to `output_buffer`. Reject if running length would exceed 9.
7. After loop, reject if running length is 0.
8. Return `Some(length_written)`.

**Returns:** `Some(length)` on success, `None` on rejection.


Note: .lower() can be done in pre-screening, there is no ambiguity for B vs. b.

Note: this precludes all future issues such as " draw  " / "(draw)" /"draW" etc. There are no spaces. There are no parentheses. There is no upper case.

Note: The function that reads bytes from the .toml file is (pre-allocated memory only) set to only return a max-length, currently 16 raw bytes (after the = in the toml. There is no huge-length input issue, anything too long is rejected at read-time.

2. **`parse_move_notation(input: &[u8]) -> Result<ParsedMoveNotation, MoveValidationError>`** — converts a byte slice of player notation into a `ParsedMoveNotation` value.
- Performs "syntactic" validation (checking/converting human string into a move data-structure).
- Does not consult board state.
- Does not check whether the move is legal (just that it is a 'move' and not 'Pizza Thursday!' (which is not a chess move).

maybe:
### Function 2: `parse_move_notation`
```rust
pub fn parse_move_notation(input: &[u8]) -> Result<ParsedMoveNotation, MoveValidationError>
```
- Calls `pre_screen_and_normalize_notation_input` first; `None` → `Err(InvalidNotation)`.
- Strips trailing annotation bytes from the right: any tail of `+ # ! ?` (zero or more, in any combination). Empty residue → `Err(InvalidNotation)`.
- **Castling detection (first):** if every residue byte is in `{ b'o', b'0', b'-' }`, then strip dashes and:
- Residue length 2 → kingside (sets `is_castle_kingside = true`).
- Residue length 3 → queenside (sets `is_castle_queenside = true`).
- Otherwise → `Err(InvalidNotation)`.
- **Non-castling decoding (digit-position-first):**
- Locate all digit positions in the residue.
- Reject if digit count is 0 or > 2.
- **One digit:** that digit is the destination rank; the byte before it is the destination file; bytes before that form a preamble (optional piece letter + optional disambiguation file/rank + optional `x` capture marker).
- **Two digits:** rightmost digit is destination rank; byte before it is destination file. Classify leftmost digit by its left-neighbor:
- If left-neighbor is a file letter `a..=h` → leftmost digit is an explicit source rank (long algebraic). Populate `explicit_source_file` and `explicit_source_rank`. Bytes before the source-file letter form the preamble (optional piece letter + optional disambiguation char).
- Otherwise → leftmost digit is `disambiguation_source_rank`.
- **Promotion:** detected as a suffix following the destination square in the one-digit branch:
- Form `=X` (with `=`) or bare `X` (no `=`), where `X ∈ {q, r, b, n}`.
- `k` and `p` promotion → `Err(InvalidPromotionRequired)` (reusing the existing error variant; it is the closest semantic fit and the only promotion-related error in the enum. If you prefer, I can add a new variant — please confirm.).
- Promotion in the two-digit (long algebraic) branch is also accepted with the same forms appended.
- File range checks: file letters must be `a..=h`; rank digits must be `1..=8` (i.e., reject `0` and `9` as rank digits in non-castling residue).
- Capture flag: `is_capture = true` if and only if an `x` byte appears in the residue (outside of castling, which has already been peeled off).
- Defaults: `piece_kind = Pawn` if no piece letter is present.
or

### Function 2: `parse_move_notation`

**Signature:**
```rust
pub fn parse_move_notation(
    input: &[u8],
) -> Result<ParsedMoveNotation, MoveValidationError>
```

**Behavior:**
1. Call `pre_screen_and_normalize_notation_input` into a stack `[u8; 9]`. `None` → `Err(InvalidNotation)`.
2. Strip trailing annotation bytes (any tail of `+ # ! ?` in any combination). Empty residue → `Err(InvalidNotation)`.
3. **Castling detection first:** if every residue byte is in `{b'o', b'0', b'-'}`:
   - Strip dashes.
   - Length 2 → kingside (`is_castle_kingside = true`).
   - Length 3 → queenside (`is_castle_queenside = true`).
   - Otherwise → `Err(InvalidNotation)`.
   - Note: this leniently accepts mixed `0`/`O` and missing dashes (e.g. `OO`, `0-O`).
4. **Non-castling decoding (digit-position-first):**
   - Scan residue for digit positions.
   - Digit count 0 or > 2 → `Err(InvalidNotation)`.
   - Validate every digit is in `1..=8` for rank role (i.e., reject `0` and `9` as ranks; `9` is already not in the allowed set, `0` is allowed only in castling).
   - **One digit:**
     - Rightmost digit = destination rank.
     - Byte immediately before it = destination file (must be `a..=h`).
     - Bytes before that = preamble: optional piece letter + optional disambiguation file/rank + optional `x` capture marker.
     - Bytes after the destination = optional promotion suffix.
   - **Two digits:**
     - Rightmost digit = destination rank; byte before = destination file.
     - Classify leftmost digit by its left-neighbor:
       - File letter `a..=h` → long algebraic: explicit source file + explicit source rank. Bytes before form a preamble (optional piece letter + optional disambiguation char).
       - Otherwise → `disambiguation_source_rank`.
     - Bytes between source square and destination square may include optional separators `-` (cosmetic) and `x` (sets `is_capture`).
     - Bytes after the destination = optional promotion suffix.
5. **Promotion suffix forms accepted** (after the destination square):
   - `=X` where `X ∈ {q, r, b, n}`.
   - Bare `X` where `X ∈ {q, r, b, n}` (no `=`).
   - `X = k` or `X = p` → `Err(InvalidPromotionPieceKind)`.
6. **Capture flag:** `is_capture = true` iff an `x` byte appears in the residue (outside castling).
7. **Default piece kind:** `Pawn` if no piece letter present.
8. **Leading `p` accepted** as explicit pawn indicator.
9. Populate `ParsedMoveNotation`; unspecified fields take defaults (`None`, `false`, `Pawn`).



3. **`parse_non_move_player_command(input: &[u8]) -> Option<NonMovePlayerCommand>`** — checks if the input is `"draw"` or `"resign"` (case-insensitive). Returns `Option` because the absence of a non-move command is not an error — the absence of Non-Move-Command means the caller should try `parse_move_notation` next.

maybe:
### Function 3: `parse_non_move_player_command`
```rust
pub fn parse_non_move_player_command(input: &[u8]) -> Option<NonMovePlayerCommand>
```
- Calls `pre_screen_and_normalize_notation_input`; `None` → `None`.
- Normalized bytes `== b"draw"` → `Some(Draw)`.
- Normalized bytes `== b"resign"` → `Some(Resign)`.
- Otherwise `None`.
or
### Function 3: `parse_non_move_player_command`

**Signature:**
```rust
pub fn parse_non_move_player_command(
    input: &[u8],
) -> Option<NonMovePlayerCommand>
```

**Behavior:**
1. Call `pre_screen_and_normalize_notation_input` into a stack `[u8; 9]`. `None` → `None`.
2. Normalized bytes `== b"draw"` → `Some(NonMovePlayerCommand::Draw)`.
3. Normalized bytes `== b"resign"` → `Some(NonMovePlayerCommand::Resign)`.
4. Otherwise → `None`.

## Allowed-Character Set (for pre-screen)




### Architectural Position
This function sits between the **file ingestion layer** (which extracts `text_message` from a TOML file) and the **resolve/validate layer** (which checks the parsed move against the board). It is purely a syntactic decoder. By separating syntactic parsing from semantic validation, we get:
- A parser that is independently testable without any board state
- A clear error boundary: `InvalidNotation` means "I cannot read this," vs. `InvalidNoMatchingLegalMove` means "I read it but it is not a legal move"
- A pipeline the TUI layer can compose cleanly
---
## Notation Forms to Accept (from your spec)
### Pawn Moves
- `e4` — pawn advance, file letter + rank digit
- `exd5` — pawn capture
- `e8=Q` — pawn promotion with piece designation
- `exd8=Q` — pawn capture with promotion
### Piece Moves
- `Nf3` — piece + destination
- `Bxc6` — piece + capture + destination
- `Rac1` — disambiguation by source file
- `R1c3` — disambiguation by source rank
- `N3d2` — disambiguation by source rank (the spec example)
- `Qa1b2` — disambiguation by full source square (rare but legal)
### Long Algebraic
- `e2e4` — full from-square + to-square, no separator
- `e2-e4` — with hyphen separator
- `e4xd5` — with `x` separator (sets capture flag)
- `Ng1-f3` — piece letter prefix allowed
- `Bf3xc6` — piece letter prefix with capture
### Castling
- `O-O`, `O-O-O` (letter O, the formal form)
- `0-0`, `0-0-0` (digit zero, common shorthand)
### Suffixes (Accept and Discard)
- `+` — check marker
- `#` — checkmate marker
- `!`, `?`, `!!`, `??`, `!?`, `?!` — annotation marks
You confirmed earlier:
- `+` and `#` are accepted and discarded
- Letter-O and digit-0 castling are both accepted
- Both `-` (cosmetic) and `x` (sets capture flag) are accepted as optional separators in long algebraic forms
---
## Specific Questions Before I Code

### Whitespace handling
- no notation requires whitespace, so strip it


### Case sensitivity: force lower case
Standard chess notation uses **uppercase for pieces** (`K Q R B N`) and **lowercase for files** (`a-h`).
- `"E4"` — uppercase file letter
- `"nf3"` — lowercase knight
- `"O-O"` vs `"o-o"`
- `"DRAW"` vs `"draw"`
Two reasonable policies:
- **(A) Strict** — pieces must be uppercase, files must be lowercase, castling must be uppercase `O`. Reject all variants. Promotion piece (`=Q`) must be uppercase.
- **(B) Lenient** — accept any case for the piece letter, file letter, castling, and special commands. Players writing TOML by hand will frequently get this wrong.

Aside from usability improvement for user, another reason for "lenient" is to make text processing uniform. This is consistent with the NLP-standard two (in pseudocode)
- .lower() and
- .strip()

No known b-bishop vs. b-file ambiguity

e.g.
"b4"    → position 1 is '4' (digit)  → b is a file → pawn move
"bxc4"  → position 1 is 'x'         → b is a file → pawn capture
"bc4"   → position 1 is 'c' (letter) → b is a piece → Bishop to c4
"Bc4"   → position 1 is 'c' (letter) → b is a piece → Bishop to c4


Note: e8b is NOT ambiguous, the only possible meaning is: pawn promotes to bishop on e8. There is NO known conflict between multiple valid meanings here.

Note:
The absence of P in standard notation does not mean that if a user writes pe4 (meaning "pawn to e4") we should reject it as invalid.
If the input has a leading p then treat it as an explicit pawn-move indicator and set piece_kind = Pawn. This is consistent with accepting leading k, q, r, n, b — all piece letters, including the one that happens to be rare in practice.
The only restriction on P is promotion:
promotion_piece_kind cannot be King or Pawn (i.e., reject e8=K and e8=P).



### Promotion piece restrictions: =k, =p
- reject at the parser level


### Q4: Promotion separator
Some sources allow:
- `e8=Q` (with equals sign — most common)
- `e8Q` (no separator — older notation)
- `e8(Q)` (parenthesized) -> remove parentheses, becomes: e8Q

Does e8{letter} There are no known issues with this.
- e8b only has one meaning.
- `e8q` (after lowercasing) → pawn promotes to queen on e8. The dispatcher distinguishes this from long algebraic because long algebraic requires the trailing two characters to be a *file letter + digit* pair; `q` is not a file letter, so the rule does not collide.


e.g.
FormAmbiguous?Notese8=QNo= is unambiguous delimitere8QNoQ,R,N not file letterse8BNoB not a rank digit; long algebraic needs 4 charse8b (lenient)NoSame reasoning as e8B after normalizatione8b3Resolvable4 chars → long algebraic wins; 3 chars → promotion

unless there is a known-collision:
1. remove remove parentheses
2. allow = or 3-char promotion

### ASCII  Only
1. only ascii is used (no other characters are in notation かな？　うそ！


### Q5: Input size bound
What is the maximum notation length we will accept? The longest reasonable notation is something like `"Qa1xb2=Q#"` which is 9 bytes.
2. after remove all spaces and all parentheses
3. if the max is 9, then reject anything more than length==9

### 0 versus O mixing
- e.g. 0-O, O-0
I think this may be no more difficult to accept than to reject, either involves identifying it. Why not accept it?

Also note: I have seen some sources omit the dash '-'

Possibly ~two simple rules for screening (and then one to decide):
(for pre-screening)
1. Bool: contains only (each char is in) ['0''O''-']? (true is ok)
2. remove dash '-'
2. Bool: is length 2 or 3? if so: some valid notation

For specific final parsing:
1. Bool: contains only (each char is in) ['0''O''-']? (true is ok)
2. remove dash '-'
2. Bool: is length 2? -> short castle valid notation
3. Bool: is length 3? -> long castle valid notation
4. else: reject

There will always be rare edge cases but this should cover most cases with few collisions.
Sure, maybe the user types "OO-" instead of "O-O", but why not accept that? It is unlike any other notation or language-meaning.
I say: a few simple rules handles castles


### Disambiguation appearing with long algebraic:
/// # Redundant Disambiguation with Explicit Source Squares
///
/// ## Policy
///
/// This parser accepts notation in which the player provides **both** a
/// disambiguation character (file or rank) **and** an explicit source
/// square (long algebraic form) for the same move. Example: `Nbg1f3`
/// means "the knight (with disambiguation hint 'b-file') from g1 to f3".
///
/// The disambiguation and the explicit source may agree, disagree, or be
/// independent — this parser does not check. Both fields are recorded in
/// the returned `ParsedMoveNotation`:
///
/// - `disambiguation_source_file` / `disambiguation_source_rank` capture
///   whatever disambiguation character(s) the player wrote.
/// - `explicit_source_file` / `explicit_source_rank` capture the source
///   square if a complete source square was provided.
///
/// The downstream resolution layer treats the explicit source square as
/// authoritative when present. Disambiguation fields are advisory in that
/// case. This means a notation like `Nbg1f3`, where the disambiguation
/// `b` does not match the explicit source `g1`, will be resolved using
/// the `g1` source and the `b` will be ignored. A future resolver could
/// optionally surface a warning, but this parser does not.
///
/// ## Rationale
///
/// The parser is purely syntactic. Whether a player's notation is
/// internally consistent is a semantic question that belongs to the
/// resolution layer. Rejecting redundant-but-parseable input would
/// contradict the lenient-acceptance policy applied elsewhere in this
/// module (whitespace stripping, optional separators, case-insensitivity,
/// optional promotion delimiter, etc.). More-detailed input is not worse
/// input.
///
/// ## How the Parser Distinguishes These Cases
///
/// The parser uses a **digit-position-first** decoding strategy. After
/// normalization (whitespace, parens, suffixes, and castling all handled
/// separately), every digit in the residue is a chess rank — digits never
/// appear in any other role in standard notation. The parser scans the
/// normalized buffer once to locate digit positions, then dispatches on
/// digit count and digit-neighbor structure:
///
/// - **One digit:** the move provides only a destination square. No
///   explicit source is present. The byte preceding the digit is the
///   destination file; bytes before that form a preamble that may contain
///   a piece letter, a disambiguation file, and/or a capture marker.
///
/// - **Two digits:** the rightmost digit is the destination rank (and the
///   byte before it is the destination file). The leftmost digit is
///   classified by examining the byte immediately to its left:
///   - **If that byte is a file letter (`a`–`h`)**, the leftmost digit
///     is the rank component of an **explicit source square**.
///     `explicit_source_file` and `explicit_source_rank` are populated.
///     Any further bytes to the left of that source-file letter form a
///     preamble that may contain a piece letter and an *additional*
///     disambiguation character — which is the `Nbg1f3` case.
///   - **Otherwise**, the leftmost digit is a **disambiguation rank**.
///     `disambiguation_source_rank` is populated.
///     `explicit_source_*` remain `None`.
///
/// This rule cleanly separates the two roles a leftmost digit can play
/// without requiring lookahead, backtracking, or semantic knowledge. The
/// `Nbg1f3` form is handled as a straightforward extension of the
/// two-digit explicit-source case: the preamble for an explicit-source
/// notation may legally contain an optional disambiguation character in
/// addition to the optional piece letter.
///
/// ## Bounded Forms Accepted by This Rule
///
/// Listing the two-digit forms with explicit source squares, by preamble
/// length (the bytes before the source-file letter):
///
/// | Form        | Preamble | Meaning                                    |
/// |-------------|----------|--------------------------------------------|
/// | `e2e4`      | empty    | pawn move, source e2, dest e4              |
/// | `Ng1f3`     | `N`      | knight, source g1, dest f3                 |
/// | `Nbg1f3`    | `Nb`     | knight (file-disambig `b`), src g1, dst f3 |
/// | `N1g1f3`    | `N1`     | knight (rank-disambig `1`), src g1, dst f3 |
///
/// Optional separators (`-`, `x`) may appear between the source square
/// and the destination file in any of these forms (`Ng1-f3`, `Bf3xc6`,
/// `Nbg1xf3`, etc.). The `x` separator additionally sets `is_capture`.
///
/// ## Feasibility Note
///
/// This approach is implementable as a single pass over a fixed-size
/// stack buffer (no heap, no recursion, no backtracking). The total
/// number of distinct shapes the dispatcher must handle is small and
/// fully enumerable, which makes the parser exhaustively testable.



### Cargo-Test scope for Part 2
1. **Pre-screen acceptance/rejection** (8 tests)
   - empty, all-whitespace, oversize, non-ASCII, disallowed punctuation, mixed-case allowed, parentheses-stripped, internal-whitespace-stripped

2. **Pawn moves** (6 tests)
   - `e4`, `exd5`, `e8=Q`, `exd8=Q`, `e8Q` (no separator), rejection of `e8=K`

3. **Piece moves** (7 tests)
   - `Nf3`, `Bxc6`, `Rc1`, `Qd1`, `Ke2`, `Rac1`, `R1c3`

4. **Disambiguation** (4 tests)
   - file-only (`Rac1`), rank-only (`R1c3`), full square (`Qa1b2`), with capture (`Naxb4`)

5. **Long algebraic** (6 tests)
   - `e2e4`, `e2-e4`, `e4xd5`, `Ng1f3`, `Ng1-f3`, `Bf3xc6`

6. **Castling** (4 tests)
   - `O-O`, `O-O-O`, `0-0`, `0-0-0`, plus `O-O+` suffix

7. **Suffix stripping** (3 tests)
   - `e4+`, `Nf3#`, `e4!?`

8. **Non-move commands** (4 tests)
   - `draw`, `resign`, `DRAW`, `Resign`

9. **Rejection cases** (~5 tests)
   - `e9`, `i4`, `e8=K`, `e8=` (trailing equals), interior `+` like `e+4`

~47 tests organized into modules:
- `tests_pre_screen` (8)
- `tests_pawn_moves` (6)
- `tests_piece_moves` (7)
- `tests_disambiguation` (4)
- `tests_long_algebraic` (6)
- `tests_castling` (5)
- `tests_suffix_stripping` (3)
- `tests_non_move_commands` (4)
- `tests_rejection_cases` (5)


### permitted characters
(space and parentheses stripped)
allowed:
a-h (files)
x (notation letter)
o (for castling)
kqrnp (peices)
w s i g (other letters from draw, resign)
0-8 (ranks)
= - + # ! ? (notation symbol)

(others?)

By category:
[0, 1, 2, 3, 4, 5, 6, 7, 8]
[=, -, +, #, !, ?]
[a, b, c, d, e, f, g, h, x, o, k, q, r, n, p, w, s, i, g]

or as all_allowed_chars:
[0, 1, 2, 3, 4, 5, 6, 7, 8, =, -, +, #, !, ?, a, b, c, d, e, f, g, h, x, o, k, q, r, n, p, w, s, i, g]


### Rejection Cases:
#### 1. rejection before deeper analysis (maybe a separate pre-function)
- Empty input
- maybe: reject if contains any non-allowed/permitted char
- All-whitespace input (same as empty after removing spaces)
- Over-9-character input after stripping
- Non-ASCII byte in input (or any character that isn't in permitted-list?)


#### 2. rejection after analysis? (are these just sample examples?)
- Invalid promotion piece (e8=K, e8=P)
- Pawn on impossible rank (syntactically: e9, e0)
- File out of range (i4, z7)
- trailing equals: empty piece promotion (MoveValidationError::InvalidNotation)
- Promotion on non-back-rank


# possible Pipeline:
1. Call `pre_screen_and_normalize_notation_input` into a stack `[u8; 9]`. On `None`, return `Err(InvalidNotation)`.
2. Operate on the normalized slice (length 1..=9, lowercase, no whitespace, no parens).
3. Strip trailing annotation/check/mate markers (`+`, `#`, `!`, `?`) from the right end. Multiple suffix bytes allowed (e.g. `!?`, `!!`, `??`, `!?`, `?!`).
4. Dispatch:
- **Castling check first**: if every remaining byte is in `{b'o', b'0', b'-'}` AND length is 3 or 5, classify as kingside (3) or queenside (5). Otherwise no castle.
- **Non-castling**: apply the digit-position-first decoding strategy you documented:
- Scan for digit positions (excluding any digit that is part of castling — already handled).
- One digit → destination only.
- Two digits → rightmost is destination rank; classify leftmost by neighbor.
- Zero digits or >2 digits → `InvalidNotation`.
5. Within each branch:
- Identify optional leading piece letter (`k q r b n p`).
- Identify optional disambiguation char.
- Identify optional separator(s) `x` / `-` between source square and destination (long algebraic forms).
- Identify optional promotion suffix: `=X`, or bare `X` (where X ∈ `q r b n`) following a destination square that puts a pawn on rank 0 or 7.
- Reject promotion to king or pawn.
6. Populate `ParsedMoveNotation` fields. Leave fields not provided by notation as their defaults (`None` / `false` / `Pawn`).
**Important constraint I will enforce at parser level:**
- The `is_capture` flag is set if and only if an `x` separator appeared in the notation.
- The parser does **not** consult board state. A notation like `e5` (which may or may not be legal) parses successfully as a pawn move to e5; legality is checked downstream.

### Function 3: `parse_non_move_player_command`

```rust
pub fn parse_non_move_player_command(input: &[u8]) -> Option<NonMovePlayerCommand>
```

**Behavior:**
1. Call `pre_screen_and_normalize_notation_input` into a stack `[u8; 9]`. On `None`, return `None`.
2. Compare normalized bytes against `b"draw"` → `Some(Draw)`.
3. Compare normalized bytes against `b"resign"` → `Some(Resign)`.
4. Otherwise return `None`.

The caller's flow is: call `parse_non_move_player_command` first; if `None`, call `parse_move_notation`.

...

# Code for reference:

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


```

# Function for reading Toml, and how to call it:

## buffer size for reading is not hardcoded anywhere in the module.

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

Note:
case by case for need, these can be changed (to be larger or smaller-efficient values)
