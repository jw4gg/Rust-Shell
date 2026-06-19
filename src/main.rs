#[allow(unused_imports)]
use std::env;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process;
use std::rc::Rc;
use std::cell::RefCell;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

struct ShellHelper {
    completions: Rc<RefCell<HashMap<String, String>>>,
}

impl Hinter for ShellHelper {
    type Hint = String;
}
impl Highlighter for ShellHelper {}
impl Validator for ShellHelper {}
impl Helper for ShellHelper {}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let word_start = line[..pos].rfind(' ').map(|i| i + 1).unwrap_or(0);
        let prefix = &line[word_start..pos];

        let mut matches = if word_start == 0 {
            complete_command(prefix)
        } else {
            let cmd = line.split_whitespace().next().unwrap_or("");
            if let Some(script) = self.completions.borrow().get(cmd).cloned() {
                let prev = line[..word_start].split_whitespace().last().unwrap_or("");
                run_completer(&script, cmd, prefix, prev, line, pos)
            } else {
                complete_filename(prefix)
            }
        };

        matches.sort_by(|a, b| a.replacement.cmp(&b.replacement));
        matches.dedup_by(|a, b| a.replacement == b.replacement);

        Ok((word_start, matches))
    }
}

fn run_completer(
    script: &str,
    cmd: &str,
    prefix: &str,
    prev: &str,
    line: &str,
    pos: usize,
) -> Vec<Pair> {
    let Ok(output) = process::Command::new(script)
        .arg(cmd)
        .arg(prefix)
        .arg(prev)
        .env("COMP_LINE", line)
        .env("COMP_POINT", pos.to_string())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| l.starts_with(prefix))
        .map(|l| Pair {
            display: l.to_string(),
            replacement: format!("{} ", l),
        })
        .collect()
}

fn complete_command(prefix: &str) -> Vec<Pair> {
    let builtins = ["echo", "exit"];
    let mut matches: Vec<Pair> = builtins
        .iter()
        .filter(|b| b.starts_with(prefix))
        .map(|b| Pair {
            display: b.to_string(),
            replacement: format!("{} ", b),
        })
        .collect();

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let Some(fname) = fname.to_str() else {
                    continue;
                };
                if !fname.starts_with(prefix) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                        matches.push(Pair {
                            display: fname.to_string(),
                            replacement: format!("{} ", fname),
                        });
                    }
                }
            }
        }
    }
    matches
}

fn complete_filename(prefix: &str) -> Vec<Pair> {
    let mut matches = Vec::new();
    let (dir, fprefix) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let read_dir = if dir.is_empty() { "." } else { dir };
    let Ok(entries) = fs::read_dir(read_dir) else {
        return matches;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(fname) = fname.to_str() else {
            continue;
        };
        if !fname.starts_with(fprefix) {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let (suffix, dsuffix) = if is_dir { ("/", "/") } else { (" ", "") };
        matches.push(Pair {
            display: format!("{}{}", fname, dsuffix),
            replacement: format!("{}{}{}", dir, fname, suffix),
        });
    }
    matches
}

fn main() {
    let config = rustyline::Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl: Editor<ShellHelper, rustyline::history::DefaultHistory> =
        Editor::with_config(config).unwrap();

    let completions: Rc<RefCell<HashMap<String, String>>> =
        Rc::new(RefCell::new(HashMap::new()));
    rl.set_helper(Some(ShellHelper {
        completions: Rc::clone(&completions),
    }));

    loop {
        let input = match rl.readline("$ ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(_) => break,
        };

        if input.trim().is_empty() {
            continue;
        }

        let all_tokens = tokenize(&input);
        if all_tokens.is_empty() {
            continue;
        }
        let (tokens, stdout_file, stderr_file) = split_redirect(all_tokens);
        if tokens.is_empty() {
            continue;
        }
        let name = tokens[0].as_str();
        let args = &tokens[1..];

        let is_builtin = matches!(
            name,
            "exit" | "echo" | "pwd" | "cd" | "type" | "complete" | "jobs"
        );
        let mut out: Box<dyn Write> = match &stdout_file {
            Some(r) if is_builtin => match open_redirect(r) {
                Ok(f) => Box::new(f),
                Err(e) => {
                    eprintln!("{}: {}", r.path, e);
                    continue;
                }
            },
            _ => Box::new(io::stdout()),
        };
        let mut err: Box<dyn Write> = match &stderr_file {
            Some(r) if is_builtin => match open_redirect(r) {
                Ok(f) => Box::new(f),
                Err(e) => {
                    eprintln!("{}: {}", r.path, e);
                    continue;
                }
            },
            _ => Box::new(io::stderr()),
        };

        match name {
            "exit" => {
                let code = args.first().and_then(|c| c.parse().ok()).unwrap_or(0);
                process::exit(code);
            }
            "echo" => {
                writeln!(out, "{}", args.join(" ")).unwrap();
            }
            "pwd" => {
                let cwd = env::current_dir().unwrap();
                writeln!(out, "{}", cwd.display()).unwrap();
            }
            "cd" => {
                let arg = args.first().map(|s| s.as_str()).unwrap_or("~");
                let dir = if arg == "~" {
                    env::var("HOME").unwrap_or_default()
                } else {
                    arg.to_string()
                };
                if env::set_current_dir(&dir).is_err() {
                    writeln!(err, "cd: {}: No such file or directory", arg).unwrap();
                }
            }
            "jobs" => {}
            "complete" => match args.first().map(|s| s.as_str()) {
                Some("-C") => {
                    if let (Some(path), Some(cmd)) = (args.get(1), args.get(2)) {
                        completions.borrow_mut().insert(cmd.clone(), path.clone());
                    }
                }
                Some("-r") => {
                    if let Some(cmd) = args.get(1) {
                        completions.borrow_mut().remove(cmd);
                    }
                }
                Some("-p") => {
                    if let Some(cmd) = args.get(1) {
                        match completions.borrow().get(cmd) {
                            Some(path) => {
                                writeln!(out, "complete -C '{}' {}", path, cmd).unwrap()
                            }
                            None => writeln!(
                                err,
                                "complete: {}: no completion specification",
                                cmd
                            )
                            .unwrap(),
                        }
                    }
                }
                _ => {}
            },
            "type" => {
                let target = args.first().map(|s| s.as_str()).unwrap_or("");
                match target {
                    "echo" | "exit" | "type" | "pwd" | "cd" | "complete" | "jobs" => {
                        writeln!(out, "{} is a shell builtin", target).unwrap();
                    }
                    _ => match find_in_path(target) {
                        Some(path) => writeln!(out, "{} is {}", target, path.display()).unwrap(),
                        None => writeln!(err, "{}: not found", target).unwrap(),
                    },
                }
            }
            _ => match find_in_path(name) {
                Some(_) => {
                    let mut cmd = process::Command::new(name);
                    cmd.args(args);
                    if let Some(r) = &stdout_file {
                        match open_redirect(r) {
                            Ok(f) => {
                                cmd.stdout(f);
                            }
                            Err(e) => {
                                eprintln!("{}: {}", r.path, e);
                                continue;
                            }
                        }
                    }
                    if let Some(r) = &stderr_file {
                        match open_redirect(r) {
                            Ok(f) => {
                                cmd.stderr(f);
                            }
                            Err(e) => {
                                eprintln!("{}: {}", r.path, e);
                                continue;
                            }
                        }
                    }
                    if let Err(e) = cmd.status() {
                        eprintln!("{}: failed to execute: {}", name, e);
                    }
                }
                None => writeln!(err, "{}: command not found", name).unwrap(),
            },
        }
    }
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut in_single = false;
    let mut in_double = false;

    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                current.push(c);
            }
        } else if in_double {
            if c == '"' {
                in_double = false;
            } else if c == '\\' && matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) {
                current.push(chars.next().unwrap());
            } else {
                current.push(c);
            }
        } else if c == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
                started = true;
            }
        } else if c == '\'' {
            in_single = true;
            started = true;
        } else if c == '"' {
            in_double = true;
            started = true;
        } else if c.is_whitespace() {
            if started {
                tokens.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            current.push(c);
            started = true;
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
}

struct Redirect {
    path: String,
    append: bool,
}

fn open_redirect(r: &Redirect) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(r.append)
        .truncate(!r.append)
        .open(&r.path)
}

fn split_redirect(tokens: Vec<String>) -> (Vec<String>, Option<Redirect>, Option<Redirect>) {
    let mut args = Vec::new();
    let mut stdout_file = None;
    let mut stderr_file = None;
    let mut iter = tokens.into_iter();
    while let Some(tok) = iter.next() {
        match tok.as_str() {
            ">" | "1>" => stdout_file = iter.next().map(|p| Redirect { path: p, append: false }),
            ">>" | "1>>" => stdout_file = iter.next().map(|p| Redirect { path: p, append: true }),
            "2>" => stderr_file = iter.next().map(|p| Redirect { path: p, append: false }),
            "2>>" => stderr_file = iter.next().map(|p| Redirect { path: p, append: true }),
            _ => args.push(tok),
        }
    }
    (args, stdout_file, stderr_file)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if let Ok(meta) = fs::metadata(&candidate) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
    }
    None
}
