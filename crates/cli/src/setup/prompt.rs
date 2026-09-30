//! The question-asking layer. The wizard's decisions never read a terminal directly, so they can
//! be driven by a script in tests and by defaults under `--yes`.

use std::io::{BufRead, Write};

pub trait Prompter {
    /// Whether answers come from a person (and questions may therefore be asked).
    fn interactive(&self) -> bool;
    fn say(&mut self, text: &str);
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, String>;
    /// Asks for a secret: the answer is not echoed and a current value is never displayed.
    /// An empty answer means "keep the current value" and is returned as an empty string.
    fn ask_secret(&mut self, question: &str, has_current: bool) -> Result<String, String>;
    /// Asks until `validate` accepts the answer.
    fn ask_validated(
        &mut self,
        question: &str,
        default: Option<&str>,
        validate: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        loop {
            let answer = self.ask(question, default)?;
            match validate(&answer) {
                Ok(()) => return Ok(answer),
                Err(e) => self.say(&e),
            }
        }
    }
    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, String>;
    /// Returns the index of the chosen option; with no default an answer is required.
    fn choose(
        &mut self,
        question: &str,
        options: &[&str],
        default: Option<usize>,
    ) -> Result<usize, String>;
}

/// `--yes`: never asks, so anything that reaches it falls back to defaults.
pub struct Auto;

impl Prompter for Auto {
    fn interactive(&self) -> bool {
        false
    }
    fn say(&mut self, _: &str) {}
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, String> {
        default.map(str::to_string).ok_or_else(|| {
            format!("{question}: no value given and no default (pass the matching flag)")
        })
    }
    fn ask_secret(&mut self, question: &str, _: bool) -> Result<String, String> {
        Err(format!("{question}: cannot be asked non-interactively"))
    }
    fn confirm(&mut self, _: &str, default: bool) -> Result<bool, String> {
        Ok(default)
    }
    fn choose(
        &mut self,
        question: &str,
        _: &[&str],
        default: Option<usize>,
    ) -> Result<usize, String> {
        default.ok_or_else(|| format!("{question}: no value given and no default"))
    }
}

/// Questions on stderr, answers from stdin.
pub struct Stdio;

impl Stdio {
    fn read_line(&self) -> Result<String, String> {
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) => Err(
                "input ended before setup finished; rerun with --yes to use flags and defaults"
                    .to_string(),
            ),
            Ok(_) => Ok(line.trim().to_string()),
            Err(e) => Err(format!("cannot read input: {e}")),
        }
    }
}

/// Turns terminal echo off on stdin until dropped; a no-op when stdin is not a terminal.
struct EchoOff {
    #[cfg(unix)]
    saved: Option<libc::termios>,
}

impl EchoOff {
    fn new() -> Self {
        #[cfg(unix)]
        {
            // SAFETY: plain termios calls on fd 0 with a zeroed, then filled, struct.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) != 0 {
                    return Self { saved: None };
                }
                let saved = t;
                t.c_lflag &= !libc::ECHO;
                if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                    return Self { saved: None };
                }
                Self { saved: Some(saved) }
            }
        }
        #[cfg(not(unix))]
        Self {}
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(t) = self.saved {
            // SAFETY: restores the attributes read in `new`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
            eprintln!();
        }
    }
}

impl Prompter for Stdio {
    fn interactive(&self) -> bool {
        true
    }
    fn say(&mut self, text: &str) {
        eprintln!("\n{text}");
    }
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, String> {
        match default {
            Some(d) if !d.is_empty() => eprint!("{question} [{d}]: "),
            _ => eprint!("{question}: "),
        }
        std::io::stderr().flush().ok();
        let answer = self.read_line()?;
        Ok(if answer.is_empty() {
            default.unwrap_or("").to_string()
        } else {
            answer
        })
    }
    fn ask_secret(&mut self, question: &str, has_current: bool) -> Result<String, String> {
        if has_current {
            eprint!("{question} (input hidden; press Enter to keep the current value): ");
        } else {
            eprint!("{question} (input hidden): ");
        }
        std::io::stderr().flush().ok();
        let _guard = EchoOff::new();
        self.read_line()
    }
    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, String> {
        loop {
            eprint!("{question} [{}]: ", if default { "Y/n" } else { "y/N" });
            std::io::stderr().flush().ok();
            match self.read_line()?.to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => eprintln!("Answer y or n."),
            }
        }
    }
    fn choose(
        &mut self,
        question: &str,
        options: &[&str],
        default: Option<usize>,
    ) -> Result<usize, String> {
        eprintln!("{question}");
        for (i, o) in options.iter().enumerate() {
            eprintln!("  {}) {o}", i + 1);
        }
        loop {
            match default {
                Some(d) => eprint!("Choice [{}]: ", d + 1),
                None => eprint!("Choice: "),
            }
            std::io::stderr().flush().ok();
            let a = self.read_line()?;
            if a.is_empty() {
                if let Some(d) = default {
                    return Ok(d);
                }
            }
            match a.parse::<usize>() {
                Ok(n) if (1..=options.len()).contains(&n) => return Ok(n - 1),
                _ => eprintln!("Enter a number from 1 to {}.", options.len()),
            }
        }
    }
}

#[cfg(test)]
pub use scripted::Scripted;

#[cfg(test)]
mod scripted {
    use super::Prompter;
    use std::collections::VecDeque;

    /// Feeds canned answers: text for `ask`, `y`/`n` for `confirm`, a 1-based number for `choose`.
    /// An empty answer takes the default. Every prompt is recorded so tests can check what a
    /// person would have seen.
    pub struct Scripted {
        answers: VecDeque<String>,
        said: Vec<String>,
        pub prompts: Vec<String>,
    }

    impl Scripted {
        pub fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|s| s.to_string()).collect(),
                said: Vec::new(),
                prompts: Vec::new(),
            }
        }
        pub fn said(&self) -> &[String] {
            &self.said
        }
        fn next(&mut self) -> Result<String, String> {
            self.answers
                .pop_front()
                .ok_or_else(|| "script ran out of answers".to_string())
        }
    }

    impl Prompter for Scripted {
        fn interactive(&self) -> bool {
            true
        }
        fn say(&mut self, text: &str) {
            self.said.push(text.to_string());
        }
        fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, String> {
            self.prompts
                .push(format!("{question} [{}]", default.unwrap_or("")));
            let a = self.next()?;
            Ok(if a.is_empty() {
                default.unwrap_or("").to_string()
            } else {
                a
            })
        }
        fn ask_secret(&mut self, question: &str, has_current: bool) -> Result<String, String> {
            self.prompts
                .push(format!("{question} (secret, current: {has_current})"));
            self.next()
        }
        fn confirm(&mut self, question: &str, default: bool) -> Result<bool, String> {
            self.prompts.push(question.to_string());
            Ok(match self.next()?.as_str() {
                "" => default,
                "y" => true,
                _ => false,
            })
        }
        fn choose(
            &mut self,
            question: &str,
            options: &[&str],
            default: Option<usize>,
        ) -> Result<usize, String> {
            self.prompts.push(format!("{question} {options:?}"));
            let a = self.next()?;
            if a.is_empty() {
                return default.ok_or_else(|| "an answer is required".to_string());
            }
            let n: usize = a.parse().map_err(|_| "bad choice".to_string())?;
            assert!(n >= 1 && n <= options.len());
            Ok(n - 1)
        }
    }
}
