//! The question-asking layer. The wizard's decisions never read a terminal directly, so they can
//! be driven by a script in tests and by defaults under `--yes`.

use std::io::{BufRead, Write};

pub trait Prompter {
    /// Whether answers come from a person (and questions may therefore be asked).
    fn interactive(&self) -> bool;
    fn say(&mut self, text: &str);
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, String>;
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
    /// Returns the index of the chosen option.
    fn choose(&mut self, question: &str, options: &[&str], default: usize)
        -> Result<usize, String>;
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
    fn confirm(&mut self, _: &str, default: bool) -> Result<bool, String> {
        Ok(default)
    }
    fn choose(&mut self, _: &str, _: &[&str], default: usize) -> Result<usize, String> {
        Ok(default)
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
        default: usize,
    ) -> Result<usize, String> {
        eprintln!("{question}");
        for (i, o) in options.iter().enumerate() {
            eprintln!("  {}) {o}", i + 1);
        }
        loop {
            eprint!("Choice [{}]: ", default + 1);
            std::io::stderr().flush().ok();
            let a = self.read_line()?;
            if a.is_empty() {
                return Ok(default);
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
    /// An empty answer takes the default.
    pub struct Scripted {
        answers: VecDeque<String>,
        said: Vec<String>,
    }

    impl Scripted {
        pub fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|s| s.to_string()).collect(),
                said: Vec::new(),
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
        fn ask(&mut self, _: &str, default: Option<&str>) -> Result<String, String> {
            let a = self.next()?;
            Ok(if a.is_empty() {
                default.unwrap_or("").to_string()
            } else {
                a
            })
        }
        fn confirm(&mut self, _: &str, default: bool) -> Result<bool, String> {
            Ok(match self.next()?.as_str() {
                "" => default,
                "y" => true,
                _ => false,
            })
        }
        fn choose(&mut self, _: &str, options: &[&str], default: usize) -> Result<usize, String> {
            let a = self.next()?;
            if a.is_empty() {
                return Ok(default);
            }
            let n: usize = a.parse().map_err(|_| "bad choice".to_string())?;
            assert!(n >= 1 && n <= options.len());
            Ok(n - 1)
        }
    }
}
