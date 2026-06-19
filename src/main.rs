#[allow(unused_imports)]
use std::env;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
#[cfg(unix)]
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
                    if is_executable(&meta) {
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

    let mut jobs: Vec<Job> = Vec::new();
    let mut variables: HashMap<String, String> = HashMap::new();
    let histfile = env::var("HISTFILE").ok();
    let mut history: Vec<String> = Vec::new();
    let mut last_append: usize = 0;

    if let Some(ref hf) = histfile {
        if let Ok(content) = fs::read_to_string(hf) {
            for line in content.lines() {
                history.push(line.to_string());
                let _ = rl.add_history_entry(line);
            }
        }
        last_append = history.len();
    }

    loop {
        reap_jobs(&mut jobs, &mut io::stdout());

        let input = match rl.readline("$ ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(_) => break,
        };

        if input.trim().is_empty() {
            continue;
        }

        history.push(input.clone());
        let _ = rl.add_history_entry(input.as_str());

        let mut all_tokens = tokenize(&input, &variables);
        if all_tokens.is_empty() {
            continue;
        }
        if all_tokens.iter().any(|t| t == "|") {
            run_pipeline(all_tokens);
            continue;
        }
        let background = all_tokens.last().map(|s| s.as_str()) == Some("&");
        if background {
            all_tokens.pop();
        }
        let (tokens, stdout_file, stderr_file) = split_redirect(all_tokens);
        if tokens.is_empty() {
            continue;
        }
        let name = tokens[0].as_str();
        let args = &tokens[1..];

        let is_builtin = matches!(
            name,
            "exit" | "echo" | "pwd" | "cd" | "type" | "complete" | "jobs" | "history" | "declare"
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
                save_histfile(&history);
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
            "jobs" => {
                let n = jobs.len();
                let mut remaining = Vec::new();
                for (i, mut job) in std::mem::take(&mut jobs).into_iter().enumerate() {
                    let marker = job_marker(i, n);
                    if matches!(job.child.try_wait(), Ok(Some(_))) {
                        writeln!(
                            out,
                            "[{}]{}  {:<24}{}",
                            job.number, marker, "Done", job.command
                        )
                        .unwrap();
                    } else {
                        writeln!(
                            out,
                            "[{}]{}  {:<24}{} &",
                            job.number, marker, job.status, job.command
                        )
                        .unwrap();
                        remaining.push(job);
                    }
                }
                jobs = remaining;
            }
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
            "declare" => match args.first().map(|s| s.as_str()) {
                Some("-p") => {
                    if let Some(n) = args.get(1) {
                        match variables.get(n) {
                            Some(v) => writeln!(out, "declare -- {}=\"{}\"", n, v).unwrap(),
                            None => writeln!(err, "declare: {}: not found", n).unwrap(),
                        }
                    }
                }
                Some(_) => {
                    for a in args {
                        if let Some(eq) = a.find('=') {
                            let name = &a[..eq];
                            let val = &a[eq + 1..];
                            if is_valid_identifier(name) {
                                variables.insert(name.to_string(), val.to_string());
                            } else {
                                writeln!(err, "declare: `{}': not a valid identifier", a).unwrap();
                            }
                        } else if is_valid_identifier(a) {
                            variables.entry(a.clone()).or_default();
                        } else {
                            writeln!(err, "declare: `{}': not a valid identifier", a).unwrap();
                        }
                    }
                }
                None => {}
            },
            "history" => match args.first().map(|s| s.as_str()) {
                Some("-r") => {
                    if let Some(p) = args.get(1) {
                        if let Ok(content) = fs::read_to_string(p) {
                            for line in content.lines() {
                                history.push(line.to_string());
                                let _ = rl.add_history_entry(line);
                            }
                        }
                    }
                }
                Some("-w") => {
                    if let Some(p) = args.get(1) {
                        let mut s = String::new();
                        for h in &history {
                            s.push_str(h);
                            s.push('\n');
                        }
                        if let Err(e) = fs::write(p, s) {
                            writeln!(err, "history: {}: {}", p, e).unwrap();
                        }
                    }
                }
                Some("-a") => {
                    if let Some(p) = args.get(1) {
                        match fs::OpenOptions::new().create(true).append(true).open(p) {
                            Ok(mut f) => {
                                for h in &history[last_append..] {
                                    let _ = writeln!(f, "{}", h);
                                }
                                last_append = history.len();
                            }
                            Err(e) => writeln!(err, "history: {}: {}", p, e).unwrap(),
                        }
                    }
                }
                Some(n) if n.parse::<usize>().is_ok() => {
                    let count = n.parse::<usize>().unwrap();
                    let start = history.len().saturating_sub(count);
                    for (i, h) in history.iter().enumerate().skip(start) {
                        writeln!(out, "{:>5}  {}", i + 1, h).unwrap();
                    }
                }
                _ => {
                    for (i, h) in history.iter().enumerate() {
                        writeln!(out, "{:>5}  {}", i + 1, h).unwrap();
                    }
                }
            },
            "type" => {
                let target = args.first().map(|s| s.as_str()).unwrap_or("");
                match target {
                    "echo" | "exit" | "type" | "pwd" | "cd" | "complete" | "jobs" | "history"
                    | "declare" => {
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
                    if background {
                        match cmd.spawn() {
                            Ok(child) => {
                                let number = next_job_number(&jobs);
                                println!("[{}] {}", number, child.id());
                                let pos = jobs
                                    .iter()
                                    .position(|j| j.number > number)
                                    .unwrap_or(jobs.len());
                                jobs.insert(
                                    pos,
                                    Job {
                                        number,
                                        child,
                                        command: tokens.join(" "),
                                        status: "Running".to_string(),
                                    },
                                );
                            }
                            Err(e) => eprintln!("{}: failed to execute: {}", name, e),
                        }
                    } else if let Err(e) = cmd.status() {
                        eprintln!("{}: failed to execute: {}", name, e);
                    }
                }
                None => writeln!(err, "{}: command not found", name).unwrap(),
            },
        }
    }

    save_histfile(&history);
}

fn tokenize(input: &str, variables: &HashMap<String, String>) -> Vec<String> {
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
            } else if c == '$' {
                expand_var(&mut chars, &mut current, variables);
            } else {
                current.push(c);
            }
        } else if c == '$' {
            if expand_var(&mut chars, &mut current, variables) {
                started = true;
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

fn expand_var(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    current: &mut String,
    variables: &HashMap<String, String>,
) -> bool {
    // '$' already consumed. Returns true if it appended non-empty text
    // (drives the empty-word drop in unquoted context).
    let name = if chars.peek() == Some(&'{') {
        chars.next();
        let mut n = String::new();
        while let Some(&c) = chars.peek() {
            if c == '}' {
                chars.next();
                break;
            }
            n.push(c);
            chars.next();
        }
        n
    } else {
        let mut n = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                n.push(c);
                chars.next();
            } else {
                break;
            }
        }
        n
    };
    if name.is_empty() {
        current.push('$'); // bare '$' is literal
        return true;
    }
    match variables.get(&name) {
        Some(v) if !v.is_empty() => {
            current.push_str(v);
            true
        }
        _ => false, // unset/empty -> nothing
    }
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn save_histfile(history: &[String]) {
    if let Ok(hf) = env::var("HISTFILE") {
        let mut s = String::new();
        for h in history {
            s.push_str(h);
            s.push('\n');
        }
        let _ = fs::write(hf, s);
    }
}

struct Job {
    number: u32,
    child: process::Child,
    command: String,
    status: String,
}

fn pipeline_builtin(name: &str) -> bool {
    matches!(name, "echo" | "pwd" | "type")
}

fn run_builtin_in_pipeline(
    seg: Vec<String>,
    stdin: Option<io::PipeReader>,
    stdout: Option<io::PipeWriter>,
) {
    if let Some(mut r) = stdin {
        let mut sink = Vec::new();
        let _ = r.read_to_end(&mut sink);
    }
    let mut out: Box<dyn Write> = match stdout {
        Some(w) => Box::new(w),
        None => Box::new(io::stdout()),
    };
    let name = seg[0].as_str();
    let args = &seg[1..];
    match name {
        "echo" => {
            let _ = writeln!(out, "{}", args.join(" "));
        }
        "pwd" => {
            if let Ok(cwd) = env::current_dir() {
                let _ = writeln!(out, "{}", cwd.display());
            }
        }
        "type" => {
            let target = args.first().map(|s| s.as_str()).unwrap_or("");
            match target {
                "echo" | "exit" | "type" | "pwd" | "cd" | "complete" | "jobs" | "history"
                | "declare" => {
                    let _ = writeln!(out, "{} is a shell builtin", target);
                }
                _ => match find_in_path(target) {
                    Some(p) => {
                        let _ = writeln!(out, "{} is {}", target, p.display());
                    }
                    None => {
                        let _ = writeln!(out, "{}: not found", target);
                    }
                },
            }
        }
        _ => {}
    }
}

fn run_pipeline(all_tokens: Vec<String>) {
    let segments: Vec<Vec<String>> = all_tokens
        .split(|t| t == "|")
        .map(|s| s.to_vec())
        .collect();
    let n = segments.len();
    if segments.iter().any(|s| s.is_empty()) {
        eprintln!("syntax error near `|'");
        return;
    }

    let mut readers: Vec<Option<io::PipeReader>> = Vec::with_capacity(n);
    let mut writers: Vec<Option<io::PipeWriter>> = Vec::with_capacity(n);
    readers.push(None);
    for _ in 0..n - 1 {
        let (r, w) = io::pipe().expect("failed to create pipe");
        writers.push(Some(w));
        readers.push(Some(r));
    }
    writers.push(None);

    let mut children = Vec::new();
    let mut threads = Vec::new();

    for (i, seg) in segments.into_iter().enumerate() {
        let stdin = readers[i].take();
        let stdout = writers[i].take();
        if pipeline_builtin(&seg[0]) {
            threads.push(std::thread::spawn(move || {
                run_builtin_in_pipeline(seg, stdin, stdout);
            }));
        } else {
            let mut cmd = process::Command::new(&seg[0]);
            cmd.args(&seg[1..]);
            if let Some(r) = stdin {
                cmd.stdin(process::Stdio::from(r));
            }
            if let Some(w) = stdout {
                cmd.stdout(process::Stdio::from(w));
            }
            match cmd.spawn() {
                Ok(child) => children.push(child),
                Err(_) => eprintln!("{}: command not found", seg[0]),
            }
        }
    }

    for mut child in children {
        let _ = child.wait();
    }
    for t in threads {
        let _ = t.join();
    }
}

fn next_job_number(jobs: &[Job]) -> u32 {
    let mut n = 1;
    while jobs.iter().any(|j| j.number == n) {
        n += 1;
    }
    n
}

fn job_marker(i: usize, n: usize) -> &'static str {
    if i + 1 == n {
        "+"
    } else if i + 2 == n {
        "-"
    } else {
        " "
    }
}

fn reap_jobs(jobs: &mut Vec<Job>, out: &mut dyn Write) {
    let n = jobs.len();
    let mut remaining = Vec::new();
    for (i, mut job) in std::mem::take(jobs).into_iter().enumerate() {
        if matches!(job.child.try_wait(), Ok(Some(_))) {
            writeln!(
                out,
                "[{}]{}  {:<24}{}",
                job.number,
                job_marker(i, n),
                "Done",
                job.command
            )
            .unwrap();
        } else {
            remaining.push(job);
        }
    }
    *jobs = remaining;
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

fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if let Ok(meta) = fs::metadata(&candidate) {
            if is_executable(&meta) {
                return Some(candidate);
            }
        }
    }
    None
}
