# Rust-Shell

A POSIX-style shell written in Rust: builtins, quoting/escaping, parameter expansion, I/O
redirection, job control, and full programmable tab completion.

## Features

- REPL with line editing and history recall (via `rustyline`)
- Builtins: `exit`, `echo`, `pwd`, `cd` (absolute, relative, and `~`), `type`, `complete`,
  `jobs`, `history`, `declare`
- Shell variables and parameter expansion:
  - `declare NAME=VALUE` to set a variable, `declare -p NAME` to print it
  - `$VAR` and `${VAR}` expansion (unquoted and inside double quotes; literal in single quotes)
  - Unset variables expand to empty; an all-empty unquoted word is dropped
- Command history:
  - In-session list with `history`, or `history <n>` for the last `n` entries
  - `history -r <file>` read, `-w <file>` write, `-a <file>` append new entries only
  - Loaded from / saved to `$HISTFILE`; up/down arrows recall previous commands
- External programs found via `PATH` (executable-bit aware), run with arguments
- Job control: background commands with `&`, `jobs` listing, automatic reaping
- Quoting & escaping: single quotes, double quotes, and backslash escapes (inside and outside
  quotes)
- I/O redirection:
  - stdout: `>`, `1>`, append `>>` / `1>>`
  - stderr: `2>`, append `2>>`
- Pipelines: `cmd1 | cmd2 | ...` (builtins and external programs)
- Tab completion:
  - Builtins and `PATH` executables
  - Longest-common-prefix completion; bell + candidate list on ambiguous matches
  - Filenames and nested paths (files get a trailing space, directories a trailing `/`)
  - Programmable completion via `complete -C <script>` — runs the registered completer with
    `argv[1..3]` (command, current word, previous word) and `COMP_LINE` / `COMP_POINT` env vars
  - `complete -p` (print spec), `complete -r` (remove spec)

## Example

```text
$ echo 'hello   world'         # quoting preserved
hello   world
$ declare NAME=world           # set a variable
$ echo "hello $NAME"           # parameter expansion
hello world
$ ls /nonexistent 2> err.txt   # stderr redirect to file
$ echo hi > out.txt            # stdout redirect
$ type echo
echo is a shell builtin
```

## Platforms

Prebuilt binaries are published for Linux, macOS, and Windows (x86_64).

## Project Layout

- `src/main.rs` — the entire shell (REPL, tokenizer, parameter expansion, builtins, redirection,
  pipelines, job control, completion)
- `Cargo.toml` / `Cargo.lock` — crate manifest and pinned dependencies
