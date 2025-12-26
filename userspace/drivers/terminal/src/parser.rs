// Command Parser Module
//
// This module handles parsing of command line input into structured commands
// with arguments. It provides a simple tokenizer and argument parser suitable
// for the terminal's built-in commands.

/// Maximum number of arguments a command can have
pub const MAX_ARGS: usize = 16;

/// Maximum length of a single argument
pub const MAX_ARG_LENGTH: usize = 128;

/// A parsed command with its arguments
pub struct ParsedCommand<'a> {
    /// The command name (first token)
    pub command: &'a str,
    /// Command arguments
    pub args: [&'a str; MAX_ARGS],
    /// Number of arguments
    pub arg_count: usize,
}

impl<'a> ParsedCommand<'a> {
    /// Get argument by index (0-based)
    pub fn arg(&self, index: usize) -> Option<&'a str> {
        if index < self.arg_count {
            Some(self.args[index])
        } else {
            None
        }
    }

    /// Check if command matches (case-insensitive)
    pub fn is(&self, name: &str) -> bool {
        self.command.eq_ignore_ascii_case(name)
    }

    /// Check if a flag is present (e.g., "-v" or "--verbose")
    pub fn has_flag(&self, short: &str, long: &str) -> bool {
        for i in 0..self.arg_count {
            if self.args[i] == short || self.args[i] == long {
                return true;
            }
        }
        false
    }

    /// Get the value of an option (e.g., "-n 5" returns "5")
    pub fn get_option(&self, short: &str, long: &str) -> Option<&'a str> {
        for i in 0..self.arg_count.saturating_sub(1) {
            if self.args[i] == short || self.args[i] == long {
                return Some(self.args[i + 1]);
            }
        }
        None
    }

    /// Get all non-flag arguments (arguments not starting with '-')
    pub fn positional_args(&self) -> impl Iterator<Item = &'a str> {
        self.args[..self.arg_count]
            .iter()
            .filter(|a| !a.starts_with('-'))
            .copied()
    }
}

/// Parse a command line string into a structured command
pub fn parse_command(input: &str) -> Option<ParsedCommand<'_>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut args: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut arg_count = 0;
    let mut command = "";

    let mut in_quotes = false;
    let mut quote_char = ' ';
    let mut token_start = 0;
    let mut in_token = false;

    let bytes = trimmed.as_bytes();
    let len = bytes.len();

    for i in 0..=len {
        let ch = if i < len { bytes[i] as char } else { ' ' };
        let is_end = i == len;

        if in_quotes {
            if ch == quote_char {
                // End of quoted string
                in_quotes = false;
                if is_end || (i + 1 < len && bytes[i + 1].is_ascii_whitespace()) {
                    // Add token without quotes
                    let token = &trimmed[token_start..i];
                    if command.is_empty() {
                        command = token;
                    } else if arg_count < MAX_ARGS {
                        args[arg_count] = token;
                        arg_count += 1;
                    }
                    in_token = false;
                }
            }
        } else if ch == '"' || ch == '\'' {
            // Start of quoted string
            if !in_token {
                quote_char = ch;
                in_quotes = true;
                token_start = i + 1; // Skip the quote
                in_token = true;
            }
        } else if ch.is_ascii_whitespace() || is_end {
            // End of token
            if in_token {
                let token = &trimmed[token_start..i];
                if command.is_empty() {
                    command = token;
                } else if arg_count < MAX_ARGS {
                    args[arg_count] = token;
                    arg_count += 1;
                }
                in_token = false;
            }
        } else {
            // Start or continue token
            if !in_token {
                token_start = i;
                in_token = true;
            }
        }
    }

    if command.is_empty() {
        return None;
    }

    Some(ParsedCommand {
        command,
        args,
        arg_count,
    })
}

/// Split a path string into components
pub fn split_path(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// Parse a number from a string
pub fn parse_number(s: &str) -> Option<u64> {
    // Handle hex prefix
    if s.starts_with("0x") || s.starts_with("0X") {
        return u64::from_str_radix(&s[2..], 16).ok();
    }

    // Handle binary prefix
    if s.starts_with("0b") || s.starts_with("0B") {
        return u64::from_str_radix(&s[2..], 2).ok();
    }

    // Handle octal prefix
    if s.starts_with("0o") || s.starts_with("0O") {
        return u64::from_str_radix(&s[2..], 8).ok();
    }

    // Decimal
    s.parse().ok()
}

/// Simple glob pattern matching (supports * and ?)
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();

    let mut pi = 0; // pattern inde
    let mut ti = 0; // text index
    let mut star_pi = usize::MAX; // pattern index after last *
    let mut star_ti = usize::MAX; // text index after last *

    while ti < text_bytes.len() {
        if pi < pattern_bytes.len() && (pattern_bytes[pi] == b'?' || pattern_bytes[pi] == text_bytes[ti]) {
            // Match single character
            pi += 1;
            ti += 1;
        } else if pi < pattern_bytes.len() && pattern_bytes[pi] == b'*' {
            // Wildcard match
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            // Backtrack on failed match after *
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    // Skip trailing *s
    while pi < pattern_bytes.len() && pattern_bytes[pi] == b'*' {
        pi += 1;
    }

    pi == pattern_bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let cmd = parse_command("help").unwrap();
        assert_eq!(cmd.command, "help");
        assert_eq!(cmd.arg_count, 0);
    }

    #[test]
    fn test_parse_with_args() {
        let cmd = parse_command("echo hello world").unwrap();
        assert_eq!(cmd.command, "echo");
        assert_eq!(cmd.arg_count, 2);
        assert_eq!(cmd.arg(0), Some("hello"));
        assert_eq!(cmd.arg(1), Some("world"));
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.txt", "file.txt"));
        assert!(glob_match("test*", "testing"));
        assert!(glob_match("?ello", "hello"));
        assert!(!glob_match("*.txt", "file.doc"));
    }
}