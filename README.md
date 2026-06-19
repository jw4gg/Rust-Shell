shell-rust
A POSIX-style shell written in Rust: builtins, quoting/escaping, I/O redirection, and full programmable tab completion.
Features
REPL with line editing (via `rustyline`)
Builtins: `exit`, `echo`, `pwd`, `cd` (absolute, relative, and `~`), `type`, `complete`, `jobs`
External programs found via `PATH` (executable-bit aware), run with arguments
Quoting & escaping: single quotes, double quotes, and backslash escapes (inside and outside quotes)
I/O redirection:
stdout: `>`, `1>`, append `>>` / `1>>`
stderr: `2>`, append `2>>`
Tab completion:
Builtins and `PATH` executables
Longest-common-prefix completion; bell + candidate list on ambiguous matches
Filenames and nested paths (files get a trailing space, directories a trailing `/`)
Programmable completion via `complete -C <script>` — runs the registered completer with
`argv[1..3]` (command, current word, previous word) and `COMP_LINE` / `COMP_POINT` env vars
`complete -p` (print spec), `complete -r` (remove spec)
```
Example
```text
$ echo 'hello   world'         # quoting preserved
hello   world
$ ls /nonexistent 2> err.txt   # stderr redirect to file
$ echo hi > out.txt            # stdout redirect
$ type echo
echo is a shell builtin
```
Project Layout
`src/main.rs` — the entire shell (REPL, tokenizer, builtins, redirection, completion)
`Cargo.toml` / `Cargo.lock` — crate manifest and pinned dependencies
