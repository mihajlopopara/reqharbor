//! A small POSIX-shell word splitter, sufficient for pasted cURL commands
//! (AC-CURL-1, AC-CURL-2). Supports:
//!
//! * backslash-newline line continuations (LF and CRLF), outside quotes and
//!   inside double quotes;
//! * single quotes (fully literal), so `'\''` yields a literal `'`;
//! * double quotes with `\"`, `\\`, `\$` and `` \` `` escapes;
//! * ANSI-C `$'...'` quoting (`\n`, `\t`, `\r`, `\'`, `\"`, `\\`, `\xHH`,
//!   `\uHHHH`, `\UHHHHHHHH`, octal `\NNN`, `\a`, `\b`, `\e`, `\f`, `\v`);
//! * `$"..."` (treated as double quotes);
//! * `#` comments at the start of a word;
//! * unquoted `;`, `|`, `&&`, `||` words end the command.
//!
//! No expansion is performed (`$VAR` stays literal). Never panics.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// True when any part of the word was quoted or escaped.
    pub quoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShellError {
    #[error("Unterminated {0} quote")]
    UnterminatedQuote(&'static str),
}

/// Split `input` into shell words.
#[allow(unused_assignments)] // the `finish!` macro resets state on its last use too
pub fn split_words(input: &str) -> Result<Vec<Word>, ShellError> {
    let chars: Vec<char> = input.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut i = 0;
    let n = chars.len();

    macro_rules! finish {
        () => {
            if in_word {
                let w = Word {
                    text: std::mem::take(&mut cur),
                    quoted,
                };
                if !w.quoted && matches!(w.text.as_str(), ";" | "|" | "&&" | "||" | "&") {
                    return Ok(words);
                }
                words.push(w);
                in_word = false;
                quoted = false;
            }
        };
    }

    while i < n {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                finish!();
                i += 1;
            }
            '\\' => {
                // Line continuation: `\` + LF or `\` + CRLF.
                if i + 1 < n && chars[i + 1] == '\n' {
                    i += 2;
                } else if i + 2 < n && chars[i + 1] == '\r' && chars[i + 2] == '\n' {
                    i += 3;
                } else if i + 1 < n {
                    cur.push(chars[i + 1]);
                    in_word = true;
                    quoted = true;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '#' if !in_word => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\'' => {
                in_word = true;
                quoted = true;
                i += 1;
                loop {
                    if i >= n {
                        return Err(ShellError::UnterminatedQuote("single"));
                    }
                    if chars[i] == '\'' {
                        i += 1;
                        break;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                i = read_double(&chars, i + 1, &mut cur)?;
            }
            '$' if i + 1 < n && chars[i + 1] == '\'' => {
                in_word = true;
                quoted = true;
                i = read_ansi_c(&chars, i + 2, &mut cur)?;
            }
            '$' if i + 1 < n && chars[i + 1] == '"' => {
                in_word = true;
                quoted = true;
                i = read_double(&chars, i + 2, &mut cur)?;
            }
            _ => {
                cur.push(c);
                in_word = true;
                i += 1;
            }
        }
    }
    finish!();
    Ok(words)
}

/// Read a double-quoted string starting after the opening quote. Returns the
/// index after the closing quote.
fn read_double(chars: &[char], mut i: usize, out: &mut String) -> Result<usize, ShellError> {
    let n = chars.len();
    loop {
        if i >= n {
            return Err(ShellError::UnterminatedQuote("double"));
        }
        match chars[i] {
            '"' => return Ok(i + 1),
            '\\' if i + 1 < n => {
                let next = chars[i + 1];
                match next {
                    '"' | '\\' | '$' | '`' => {
                        out.push(next);
                        i += 2;
                    }
                    '\n' => i += 2,
                    '\r' if i + 2 < n && chars[i + 2] == '\n' => i += 3,
                    _ => {
                        out.push('\\');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
}

fn hex_value(chars: &[char], start: usize, max: usize) -> (u32, usize) {
    let mut v = 0u32;
    let mut len = 0;
    while len < max && start + len < chars.len() {
        match chars[start + len].to_digit(16) {
            Some(d) => {
                v = v.wrapping_mul(16).wrapping_add(d);
                len += 1;
            }
            None => break,
        }
    }
    (v, len)
}

/// Read an ANSI-C `$'...'` string starting after `$'`.
fn read_ansi_c(chars: &[char], mut i: usize, out: &mut String) -> Result<usize, ShellError> {
    let n = chars.len();
    // Collect raw bytes so `\xHH` sequences can form UTF-8 together.
    let mut bytes: Vec<u8> = Vec::new();
    let push_char = |bytes: &mut Vec<u8>, c: char| {
        let mut buf = [0u8; 4];
        bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    };
    loop {
        if i >= n {
            return Err(ShellError::UnterminatedQuote("$'"));
        }
        let c = chars[i];
        if c == '\'' {
            i += 1;
            break;
        }
        if c != '\\' || i + 1 >= n {
            push_char(&mut bytes, c);
            i += 1;
            continue;
        }
        let e = chars[i + 1];
        i += 2;
        match e {
            'n' => bytes.push(b'\n'),
            't' => bytes.push(b'\t'),
            'r' => bytes.push(b'\r'),
            'a' => bytes.push(0x07),
            'b' => bytes.push(0x08),
            'e' | 'E' => bytes.push(0x1b),
            'f' => bytes.push(0x0c),
            'v' => bytes.push(0x0b),
            '\\' => bytes.push(b'\\'),
            '\'' => bytes.push(b'\''),
            '"' => bytes.push(b'"'),
            '?' => bytes.push(b'?'),
            'x' => {
                let (v, len) = hex_value(chars, i, 2);
                if len == 0 {
                    bytes.extend_from_slice(b"\\x");
                } else {
                    bytes.push(v as u8);
                    i += len;
                }
            }
            'u' | 'U' => {
                let max = if e == 'u' { 4 } else { 8 };
                let (v, len) = hex_value(chars, i, max);
                match (len, char::from_u32(v)) {
                    (1.., Some(ch)) => {
                        push_char(&mut bytes, ch);
                        i += len;
                    }
                    _ => {
                        bytes.push(b'\\');
                        push_char(&mut bytes, e);
                    }
                }
            }
            'c' if i < n => {
                bytes.push((chars[i] as u32 & 0x1f) as u8);
                i += 1;
            }
            '0'..='7' => {
                let mut v = e.to_digit(8).unwrap_or(0);
                let mut len = 0;
                while len < 2 && i < n {
                    match chars[i].to_digit(8) {
                        Some(d) => {
                            v = v * 8 + d;
                            i += 1;
                            len += 1;
                        }
                        None => break,
                    }
                }
                bytes.push((v & 0xff) as u8);
            }
            other => {
                bytes.push(b'\\');
                push_char(&mut bytes, other);
            }
        }
    }
    out.push_str(&String::from_utf8_lossy(&bytes));
    Ok(i)
}

/// Quote a value for POSIX shells: single quotes, with `'` written as `'\''`
/// (AC-CEXP-3).
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    out.push_str(&s.replace('\'', "'\\''"));
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        split_words(s)
            .unwrap()
            .into_iter()
            .map(|w| w.text)
            .collect()
    }

    #[test]
    fn ac_curl_1_line_continuations_lf_and_crlf() {
        let single = words("curl 'http://x' -H 'A: b' -d 'x'");
        assert_eq!(
            words("curl 'http://x' \\\n  -H 'A: b' \\\n  -d 'x'"),
            single
        );
        assert_eq!(
            words("curl 'http://x' \\\r\n  -H 'A: b' \\\r\n  -d 'x'"),
            single
        );
        // continuation in the middle of a word joins it
        assert_eq!(words("cu\\\nrl"), vec!["curl"]);
        // inside double quotes too
        assert_eq!(words("\"a\\\nb\""), vec!["ab"]);
        // but not inside single quotes
        assert_eq!(words("'a\\\nb'"), vec!["a\\\nb"]);
    }

    #[test]
    fn ac_curl_2_quotes() {
        assert_eq!(words(r#"'a "b" \n $x'"#), vec![r#"a "b" \n $x"#]);
        assert_eq!(words(r#""a \"b\" \\ \$x \n""#), vec![r#"a "b" \ $x \n"#]);
        assert_eq!(
            words(r"$'l1\nl2\tt \'q\' \\ \x41é'"),
            vec!["l1\nl2\tt 'q' \\ Aé"]
        );
        assert_eq!(words(r"'it'\''s'"), vec!["it's"]);
        assert_eq!(words(r"a\ b c"), vec!["a b", "c"]);
        assert_eq!(words("''"), vec![""]);
        assert_eq!(words(r"$'\101\x7e\r\n'"), vec!["A~\r\n"]);
        assert_eq!(words(r#"$"dq""#), vec!["dq"]);
    }

    #[test]
    fn separators_and_comments() {
        assert_eq!(words("curl a ; curl b"), vec!["curl", "a"]);
        assert_eq!(words("curl 'a;b' x"), vec!["curl", "a;b", "x"]);
        assert_eq!(words("curl a # comment\n b"), vec!["curl", "a", "b"]);
        assert_eq!(words("curl a#b"), vec!["curl", "a#b"]);
    }

    #[test]
    fn unterminated_is_error_not_panic() {
        assert!(split_words("curl 'abc").is_err());
        assert!(split_words("curl \"abc").is_err());
        assert!(split_words("curl $'abc").is_err());
        assert!(split_words("\\").unwrap().is_empty());
    }

    #[test]
    fn quote_round_trips() {
        for s in ["", "plain", "it's", "'''", "a\nb", "$HOME \\ \"x\""] {
            assert_eq!(words(&quote(s)), vec![s.to_string()]);
        }
    }
}
