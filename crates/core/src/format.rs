//! Body formatting (AC-REQ-6): JSON pretty-print with 2 spaces and XML
//! indentation. On invalid input the original text is untouched and the error
//! position (1-based line/column) is returned.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{message} at line {line}, column {column}")]
pub struct FormatError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

/// Pretty-print JSON with 2-space indentation. Works at the token level, so
/// numbers, key order and string escapes are preserved exactly.
pub fn format_json(text: &str) -> Result<String, FormatError> {
    if let Err(e) = serde_json::from_str::<serde::de::IgnoredAny>(text) {
        let msg = e.to_string();
        let message = msg.split(" at line ").next().unwrap_or(&msg).to_string();
        return Err(FormatError {
            message,
            line: e.line(),
            column: e.column(),
        });
    }
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut indent = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let newline = |out: &mut String, indent: usize| {
        out.push('\n');
        for _ in 0..indent {
            out.push_str("  ");
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == close {
                    out.push(c);
                    out.push(close);
                    i = j;
                } else {
                    out.push(c);
                    indent += 1;
                    newline(&mut out, indent);
                }
            }
            '}' | ']' => {
                indent = indent.saturating_sub(1);
                newline(&mut out, indent);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, indent);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
        i += 1;
    }
    Ok(out)
}

/// Pretty-print XML with 2-space indentation.
pub fn format_xml(text: &str) -> Result<String, FormatError> {
    use quick_xml::events::Event;
    use quick_xml::{Reader, Writer};

    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
    let mut depth: i64 = 0;
    let mut seen_root = false;
    loop {
        let event = match reader.read_event() {
            Ok(ev) => ev,
            Err(e) => {
                return Err(xml_error(
                    text,
                    reader.error_position() as usize,
                    e.to_string(),
                ));
            }
        };
        match &event {
            Event::Eof => break,
            Event::Start(_) => {
                depth += 1;
                seen_root = true;
            }
            Event::End(_) => depth -= 1,
            Event::Empty(_) => seen_root = true,
            _ => {}
        }
        if writer.write_event(event).is_err() {
            return Err(xml_error(
                text,
                reader.buffer_position() as usize,
                "write error".into(),
            ));
        }
    }
    if depth != 0 {
        return Err(xml_error(text, text.len(), "Unclosed element".into()));
    }
    if !seen_root {
        return Err(xml_error(text, 0, "No root element".into()));
    }
    String::from_utf8(writer.into_inner()).map_err(|_| xml_error(text, 0, "Invalid UTF-8".into()))
}

fn xml_error(text: &str, offset: usize, message: String) -> FormatError {
    let (line, column) = line_col(text, offset.min(text.len()));
    FormatError {
        message,
        line,
        column,
    }
}

/// 1-based line/column of a byte offset.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, c) in text.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_req_6_json_pretty_two_spaces() {
        let out = format_json(r#"{"a":1,"b":[1,2,{}],"c":{"d":"x, y: {z}"},"e":[]}"#).unwrap();
        assert_eq!(
            out,
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2,\n    {}\n  ],\n  \"c\": {\n    \"d\": \"x, y: {z}\"\n  },\n  \"e\": []\n}"
        );
    }

    #[test]
    fn ac_req_6_json_preserves_numbers_and_escapes() {
        let out = format_json(r#"[123456789012345678901234567890, 1.10, "q\"\\"]"#).unwrap();
        assert_eq!(
            out,
            "[\n  123456789012345678901234567890,\n  1.10,\n  \"q\\\"\\\\\"\n]"
        );
    }

    #[test]
    fn ac_req_6_json_error_position() {
        let err = format_json("{\n  \"a\": 1,\n  \"b\": ?\n}").unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(err.column, 8);
    }

    #[test]
    fn ac_req_6_xml_pretty() {
        let out = format_xml("<a><b x=\"1\">t</b><c/></a>").unwrap();
        assert_eq!(out, "<a>\n  <b x=\"1\">t</b>\n  <c/>\n</a>");
    }

    #[test]
    fn ac_req_6_xml_error_position() {
        let err = format_xml("<a>\n<b></c>\n</a>").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(format_xml("<a><b></b>").is_err());
        assert!(format_xml("not xml").is_err());
    }
}
