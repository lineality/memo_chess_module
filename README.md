#### memo_chess_module

# Tiebreak
Memo-Chess is an implementation of chess designed to run on Uma's Decentralized Multipoint Conferencing Unit Distributed Graph Database, as a 'tie break' mechanism using one or more games of chess to decide (rather than a coin flip).

There is a TUI (text user interface) with minimal ascii or a Unicode-ANSI mode (also ~minimal).

Configuration and moves are .toml files in a single directory.

Game-TUI can be launched either in a new terminal or in a Tmux-split (vertical or horizontal) (or in the same terminal, mostly for demo-testing). See github repo for demo-test files.

### Includes:
- Standard chess notation
- Draw-offer
- 50 Move Rule (as a hard-rule)

#### Does not yet include:
- 3-time repetition
- different starting clocks for armageddon-rules

#### See:
- https://github.com/lineality/memo_chess_module
- https://github.com/lineality/uma_productivity_collaboration_tool
