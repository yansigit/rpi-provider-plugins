//! Node/V8-compatible `JSON.parse` diagnostics for OAuth and project bodies.
//!
//! Upstream calls `response.json()`; a malformed body therefore surfaces V8's
//! `SyntaxError` text (through `Antigravity OAuth exchange failed: ...` and the
//! bare refresh throw). `serde_json` reports a different message, so this module
//! reproduces V8's scanner and error templates exactly. It is only consulted
//! after `serde_json` rejects a body, so valid JSON never reaches it.
//!
//! Ported from V8 13.6.233 `src/json/json-parser.cc` / `json-parser.h` and
//! `src/common/message-template.h`. Lone UTF-16 surrogates cannot be represented
//! in a Rust `String`; those inputs are reported lossily (see `token`).

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Eos,
    Number,
    String,
    True,
    False,
    Null,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Colon,
    Comma,
    Whitespace,
    Illegal,
}

/// V8's `kEndOfString` sentinel for `CurrentCharacter` past the input.
const END: u32 = u32::MAX;

fn one_char_token(c: u32) -> Token {
    match c {
        c if c > 0xff => Token::Illegal,
        0x22 => Token::String,
        0x30..=0x39 => Token::Number,
        0x2d => Token::Number,
        0x5b => Token::LBrack,
        0x7b => Token::LBrace,
        0x5d => Token::RBrack,
        0x7d => Token::RBrace,
        0x74 => Token::True,
        0x66 => Token::False,
        0x6e => Token::Null,
        0x20 | 0x09 | 0x0d | 0x0a => Token::Whitespace,
        0x3a => Token::Colon,
        0x2c => Token::Comma,
        _ => Token::Illegal,
    }
}

fn is_decimal_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
}

/// V8 `IsNumberPart`: `.`, `e`, `E`, digit, `-`, `+`.
fn is_number_part(c: u32) -> bool {
    matches!(c, 0x2e | 0x65 | 0x45 | 0x2d | 0x2b) || is_decimal_digit(c)
}

#[derive(Clone, Copy)]
enum EscapeKind {
    Illegal,
    SelfChar,
    Backspace,
    Tab,
    NewLine,
    FormFeed,
    CarriageReturn,
    Unicode,
}

fn escape_kind(c: u32) -> EscapeKind {
    match c {
        0x62 => EscapeKind::Backspace,
        0x74 => EscapeKind::Tab,
        0x6e => EscapeKind::NewLine,
        0x66 => EscapeKind::FormFeed,
        0x72 => EscapeKind::CarriageReturn,
        0x75 => EscapeKind::Unicode,
        0x22 | 0x5c | 0x2f => EscapeKind::SelfChar,
        _ => EscapeKind::Illegal,
    }
}

fn hex_value(c: u32) -> i32 {
    match c {
        0x30..=0x39 => (c - 0x30) as i32,
        0x41..=0x46 => (c - 0x41 + 10) as i32,
        0x61..=0x66 => (c - 0x61 + 10) as i32,
        _ => -1,
    }
}

fn ascii_alpha_to_lower(c: u32) -> u32 {
    if (0x41..=0x5a).contains(&c) {
        c + 0x20
    } else {
        c
    }
}

/// A V8 `JsonParser<uint16_t>` restricted to the recursive (no-reviver) path.
struct Parser {
    chars: Vec<u16>,
    cursor: usize,
    next: Token,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.encode_utf16().collect(),
            cursor: 0,
            next: Token::Eos,
        }
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn at_end(&self) -> bool {
        self.cursor >= self.chars.len()
    }

    fn current(&self) -> u32 {
        if self.at_end() {
            END
        } else {
            self.chars[self.cursor] as u32
        }
    }

    fn advance(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    fn next_character(&mut self) -> u32 {
        self.advance();
        self.current()
    }

    fn skip_whitespace(&mut self) {
        while !self.at_end() && one_char_token(self.chars[self.cursor] as u32) == Token::Whitespace
        {
            self.cursor += 1;
        }
        self.next = if self.at_end() {
            Token::Eos
        } else {
            one_char_token(self.chars[self.cursor] as u32)
        };
    }

    fn consume(&mut self) {
        self.advance();
    }

    fn check(&mut self, token: Token) -> bool {
        self.skip_whitespace();
        if self.next != token {
            return false;
        }
        self.advance();
        true
    }

    fn expect(&mut self, token: Token, template: Option<&str>) -> Result<(), String> {
        if self.next == token {
            self.advance();
            Ok(())
        } else {
            Err(self.report(self.next, template))
        }
    }

    fn expect_next(&mut self, token: Token, template: Option<&str>) -> Result<(), String> {
        self.skip_whitespace();
        self.expect(token, template)
    }

    /// V8 `ReportUnexpectedToken` plus `LookUpErrorMessageForJsonToken`.
    ///
    /// `template` is one of V8's positioned templates (they end in
    /// `... in JSON at position`); `None` selects the token-specific message.
    fn report(&self, token: Token, template: Option<&str>) -> String {
        let (line, column) = self.file_location(self.cursor);
        let pos = self.cursor;
        match template {
            Some(template) => format!("{template} {pos} (line {line} column {column})"),
            None => match token {
                Token::Eos => "Unexpected end of JSON input".to_owned(),
                Token::Number => format!(
                    "Unexpected number in JSON at position {pos} (line {line} column {column})"
                ),
                Token::String => format!(
                    "Unexpected string in JSON at position {pos} (line {line} column {column})"
                ),
                _ if self.is_special_string() => {
                    format!("\"{}\" is not valid JSON", self.source())
                }
                _ => self.ellipses(),
            },
        }
    }

    fn report_character(&self, c: u32) -> String {
        let token = if c == END {
            Token::Eos
        } else if c <= 0xff {
            one_char_token(c)
        } else {
            Token::Illegal
        };
        self.report(token, None)
    }

    fn source(&self) -> String {
        String::from_utf16_lossy(&self.chars)
    }

    /// V8 `IsSpecialString`: the whole input is one of four non-JSON literals.
    fn is_special_string(&self) -> bool {
        matches!(
            self.source().as_str(),
            "[object Object]" | "undefined" | "Infinity" | "NaN"
        )
    }

    fn token(&self, pos: usize) -> String {
        match self.chars.get(pos) {
            Some(unit) => char::from_u32(*unit as u32)
                .map(|c| c.to_string())
                .unwrap_or_else(|| String::from_utf16_lossy(&[*unit])),
            None => String::new(),
        }
    }

    /// V8 `GetErrorMessageWithEllipses` (10 context characters, 21 minimum).
    fn ellipses(&self) -> String {
        const MAX: usize = 10;
        const MIN: usize = MAX * 2 + 1;
        let pos = self.cursor;
        let length = self.len();
        let token = self.token(pos);
        if length >= MIN {
            let (start, end, prefix, suffix) = if pos < MAX {
                (0, pos + MAX, "", "...")
            } else if pos < length - MAX {
                (pos - MAX, pos + MAX, "...", "...")
            } else {
                (pos - MAX, length, "...", "")
            };
            let snippet = String::from_utf16_lossy(&self.chars[start..end]);
            format!("Unexpected token '{token}', {prefix}\"{snippet}\"{suffix} is not valid JSON")
        } else {
            format!(
                "Unexpected token '{token}', \"{}\" is not valid JSON",
                self.source()
            )
        }
    }

    /// V8 `CalculateFileLocation`: `\r\n` is one terminator, columns are code units.
    fn file_location(&self, pos: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut last_break = 0usize;
        let mut cursor = 0usize;
        while cursor < pos {
            if self.chars[cursor] == 0x0d && cursor < pos - 1 && self.chars[cursor + 1] == 0x0a {
                cursor += 1;
            }
            if self.chars[cursor] == 0x0d || self.chars[cursor] == 0x0a {
                line += 1;
                last_break = cursor + 1;
            }
            cursor += 1;
        }
        (line, 1 + (pos - last_break))
    }

    fn scan_unicode_character(&mut self) -> i32 {
        let mut value = 0i32;
        for _ in 0..4 {
            let digit = hex_value(self.next_character());
            if digit < 0 {
                return -1;
            }
            value = value * 16 + digit;
        }
        value
    }

    fn scan_json_string(&mut self) -> Result<(), String> {
        loop {
            while !self.at_end() {
                let c = self.chars[self.cursor] as u32;
                if c == 0x22 || c == 0x5c || c < 0x20 {
                    break;
                }
                self.cursor += 1;
            }
            if self.at_end() {
                return Err(self.report(
                    Token::Illegal,
                    Some("Unterminated string in JSON at position"),
                ));
            }
            if self.chars[self.cursor] == 0x22 {
                self.advance();
                return Ok(());
            }
            if self.chars[self.cursor] == 0x5c {
                let c = self.next_character();
                if c > 0xff {
                    return Err(self.report_character(c));
                }
                match escape_kind(c) {
                    EscapeKind::Unicode => {
                        if self.scan_unicode_character() < 0 {
                            return Err(self.report(
                                Token::Illegal,
                                Some("Bad Unicode escape in JSON at position"),
                            ));
                        }
                    }
                    EscapeKind::Illegal => {
                        return Err(self.report(
                            Token::Illegal,
                            Some("Bad escaped character in JSON at position"),
                        ));
                    }
                    _ => {}
                }
                self.advance();
                continue;
            }
            return Err(self.report(
                Token::Illegal,
                Some("Bad control character in string literal in JSON at position"),
            ));
        }
    }

    fn advance_to_non_decimal(&mut self) {
        while !self.at_end() && is_decimal_digit(self.chars[self.cursor] as u32) {
            self.cursor += 1;
        }
    }

    fn parse_json_number(&mut self) -> Result<(), String> {
        let mut sign = 1i32;
        let mut c = self.current();
        if c == 0x2d {
            sign = -1;
            c = self.next_character();
        }
        if c == 0x30 {
            c = self.next_character();
            if c <= 0xff && is_number_part(c) {
                if is_decimal_digit(c) {
                    return Err(self.report(Token::Number, None));
                }
            } else if sign > 0 {
                return Ok(());
            }
        } else {
            let smi_start = self.cursor;
            let stop = (self.cursor + 9).min(self.len());
            while self.cursor < stop && is_decimal_digit(self.chars[self.cursor] as u32) {
                self.cursor += 1;
            }
            if smi_start == self.cursor {
                return Err(self.report(
                    Token::Illegal,
                    Some("No number after minus sign in JSON at position"),
                ));
            }
            c = self.current();
            if !(c <= 0xff && is_number_part(c)) {
                return Ok(());
            }
            self.advance_to_non_decimal();
        }
        if self.current() == 0x2e {
            c = self.next_character();
            if !is_decimal_digit(c) {
                return Err(self.report(
                    Token::Illegal,
                    Some("Unterminated fractional number in JSON at position"),
                ));
            }
            self.advance_to_non_decimal();
        }
        if ascii_alpha_to_lower(self.current()) == 0x65 {
            c = self.next_character();
            if c == 0x2d || c == 0x2b {
                c = self.next_character();
            }
            if !is_decimal_digit(c) {
                return Err(self.report(
                    Token::Illegal,
                    Some("Exponent part is missing a number in JSON at position"),
                ));
            }
            self.advance_to_non_decimal();
        }
        Ok(())
    }

    fn scan_literal(&mut self, literal: &[u16]) -> Result<(), String> {
        // `literal` excludes the NUL terminator V8 counts in `N`.
        let size = literal.len();
        let remaining = self.len() - self.cursor;
        if remaining >= size && self.chars[self.cursor + 1..self.cursor + size] == literal[1..size]
        {
            self.cursor += size;
            return Ok(());
        }
        self.advance();
        let count = (size - 1).min(remaining.saturating_sub(1));
        for index in 0..count {
            if literal[1 + index] != self.chars[self.cursor] {
                let c = self.chars[self.cursor] as u32;
                return Err(self.report_character(c));
            }
            self.cursor += 1;
        }
        Err(self.report(Token::Eos, None))
    }

    fn parse_value_recursive(&mut self) -> Result<(), String> {
        self.skip_whitespace();
        match self.next {
            Token::Number => self.parse_json_number(),
            Token::String => {
                self.consume();
                self.scan_json_string()
            }
            Token::True => self.scan_literal(&[0x74, 0x72, 0x75, 0x65]),
            Token::False => self.scan_literal(&[0x66, 0x61, 0x6c, 0x73, 0x65]),
            Token::Null => self.scan_literal(&[0x6e, 0x75, 0x6c, 0x6c]),
            Token::LBrace => self.parse_json_object(),
            Token::LBrack => self.parse_json_array(),
            _ => {
                let c = self.current();
                Err(self.report_character(c))
            }
        }
    }

    fn parse_json_object(&mut self) -> Result<(), String> {
        self.consume();
        if self.check(Token::RBrace) {
            return Ok(());
        }
        let mut first = true;
        loop {
            self.expect_next(
                Token::String,
                Some(if first {
                    "Expected property name or '}' in JSON at position"
                } else {
                    "Expected double-quoted property name in JSON at position"
                }),
            )?;
            self.scan_json_string()?;
            self.expect_next(
                Token::Colon,
                Some("Expected ':' after property name in JSON at position"),
            )?;
            self.parse_value_recursive()?;
            first = false;
            if !self.check(Token::Comma) {
                break;
            }
        }
        self.expect(
            Token::RBrace,
            Some("Expected ',' or '}' after property value in JSON at position"),
        )
    }

    fn parse_json_array(&mut self) -> Result<(), String> {
        self.consume();
        if self.check(Token::RBrack) {
            return Ok(());
        }
        self.skip_whitespace();
        while self.next == Token::Number {
            self.parse_json_number()?;
            if self.check(Token::Comma) {
                self.skip_whitespace();
                continue;
            }
            return self.expect(
                Token::RBrack,
                Some("Expected ',' or ']' after array element in JSON at position"),
            );
        }
        self.parse_value_recursive()?;
        while self.check(Token::Comma) {
            self.parse_value_recursive()?;
        }
        self.expect(
            Token::RBrack,
            Some("Expected ',' or ']' after array element in JSON at position"),
        )
    }

    fn parse(&mut self) -> Result<(), String> {
        self.parse_value_recursive()?;
        if !self.check(Token::Eos) {
            let next = self.next;
            return Err(self.report(
                next,
                Some("Unexpected non-whitespace character after JSON at position"),
            ));
        }
        Ok(())
    }
}

/// Return V8's `JSON.parse` message when `source` is not valid JSON, else `None`.
pub fn parse_error(source: &str) -> Option<String> {
    let mut parser = Parser::new(source);
    parser.parse().err()
}
