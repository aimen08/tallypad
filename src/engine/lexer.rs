//! Turns a single line of text into tokens with byte spans.

use std::ops::Range;

use super::units::currency_for_symbol;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(f64),
    Word(String),
    /// Currency symbol such as `$`; holds the ISO code.
    Currency(&'static str),
    Op(char),
    /// Quoted text, ignored by the evaluator.
    Quoted,
    /// `// comment` or a `# heading` line.
    Comment,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Range<usize>,
}

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '°' || c == 'º' || c == 'µ' || c == 'π'
}

fn is_word_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '²' || c == '³'
}

pub fn lex(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let n = chars.len();
    let end_of = |i: usize| if i < n { chars[i].0 } else { line.len() };

    if line.trim_start().starts_with('#') {
        let start = line.len() - line.trim_start().len();
        tokens.push(Token {
            tok: Tok::Comment,
            span: start..line.len(),
        });
        return tokens;
    }

    let mut i = 0;
    while i < n {
        let (pos, c) = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Comments
        if c == '/' && i + 1 < n && chars[i + 1].1 == '/' {
            tokens.push(Token {
                tok: Tok::Comment,
                span: pos..line.len(),
            });
            break;
        }

        // Quoted text
        if c == '"' || c == '“' {
            let mut j = i + 1;
            while j < n && chars[j].1 != '"' && chars[j].1 != '”' {
                j += 1;
            }
            let end = if j < n { end_of(j + 1) } else { line.len() };
            tokens.push(Token {
                tok: Tok::Quoted,
                span: pos..end,
            });
            i = (j + 1).min(n);
            continue;
        }

        // Numbers
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].1.is_ascii_digit()) {
            let (value, next) = lex_number(&chars, i);
            let mut value = value;
            let mut j = next;
            // Scale suffixes glued to the number: 5k, 2M, 3bn
            if j < n {
                let suffix_end = {
                    let mut k = j;
                    while k < n && chars[k].1.is_alphabetic() {
                        k += 1;
                    }
                    k
                };
                let suffix: String = chars[j..suffix_end].iter().map(|(_, c)| *c).collect();
                let scale = match suffix.as_str() {
                    "k" | "K" => Some(1e3),
                    "M" => Some(1e6),
                    "bn" => Some(1e9),
                    _ => None,
                };
                if let Some(s) = scale {
                    value *= s;
                    j = suffix_end;
                }
            }
            tokens.push(Token {
                tok: Tok::Num(value),
                span: pos..end_of(j),
            });
            i = j;
            continue;
        }

        // Words
        if is_word_start(c) {
            let mut j = i + 1;
            while j < n
                && (is_word_continue(chars[j].1)
                    // Contractions stay one word: I'm, don't, it’s
                    || (matches!(chars[j].1, '\'' | '’')
                        && j + 1 < n
                        && chars[j + 1].1.is_alphabetic()))
            {
                j += 1;
            }
            let word: String = chars[i..j].iter().map(|(_, c)| *c).collect();
            tokens.push(Token {
                tok: Tok::Word(word),
                span: pos..end_of(j),
            });
            i = j;
            continue;
        }

        if let Some(code) = currency_for_symbol(c) {
            tokens.push(Token {
                tok: Tok::Currency(code),
                span: pos..end_of(i + 1),
            });
            i += 1;
            continue;
        }

        let op = match c {
            '+' | '-' | '*' | '/' | '^' | '(' | ')' | '=' | '!' | '%' | ':' | ',' => Some(c),
            '×' | '·' | '∙' => Some('*'),
            '÷' => Some('/'),
            '−' | '–' => Some('-'),
            '[' | '{' => Some('('),
            ']' | '}' => Some(')'),
            _ => None,
        };
        if let Some(op) = op {
            // `**` is power
            if op == '*' && i + 1 < n && chars[i + 1].1 == '*' {
                tokens.push(Token {
                    tok: Tok::Op('^'),
                    span: pos..end_of(i + 2),
                });
                i += 2;
                continue;
            }
            tokens.push(Token {
                tok: Tok::Op(op),
                span: pos..end_of(i + 1),
            });
        }
        // Anything else is ignored text.
        i += 1;
    }
    tokens
}

/// Parses a number starting at `chars[i]`. Returns the value and the index
/// just past it. Supports `0x`, `0b`, `0o`, thousands separators (`1,000`),
/// underscores, decimals and exponents.
fn lex_number(chars: &[(usize, char)], i: usize) -> (f64, usize) {
    let n = chars.len();
    let at = |k: usize| if k < n { Some(chars[k].1) } else { None };

    if at(i) == Some('0') {
        let radix = match at(i + 1) {
            Some('x') | Some('X') => Some(16),
            Some('b') | Some('B') => Some(2),
            Some('o') | Some('O') => Some(8),
            _ => None,
        };
        if let Some(radix) = radix {
            let mut j = i + 2;
            let mut digits = String::new();
            while let Some(c) = at(j) {
                if c.is_digit(radix) {
                    digits.push(c);
                } else if c != '_' {
                    break;
                }
                j += 1;
            }
            if !digits.is_empty()
                && let Ok(v) = u64::from_str_radix(&digits, radix)
            {
                return (v as f64, j);
            }
        }
    }

    let mut s = String::new();
    let mut j = i;
    // Integer part with optional grouping separators.
    while let Some(c) = at(j) {
        if c.is_ascii_digit() {
            s.push(c);
            j += 1;
        } else if c == '_' && at(j + 1).is_some_and(|d| d.is_ascii_digit()) {
            j += 1;
        } else if c == ','
            && !s.is_empty()
            && (1..=3).all(|k| at(j + k).is_some_and(|d| d.is_ascii_digit()))
            && !at(j + 4).is_some_and(|d| d.is_ascii_digit())
        {
            // 1,000 style grouping: comma followed by exactly three digits.
            j += 1;
        } else {
            break;
        }
    }
    if at(j) == Some('.') && at(j + 1).is_some_and(|d| d.is_ascii_digit()) {
        s.push('.');
        j += 1;
        while let Some(c) = at(j) {
            if c.is_ascii_digit() {
                s.push(c);
                j += 1;
            } else {
                break;
            }
        }
    }
    // Exponent: only when followed by digits (so `2e` stays 2 * e).
    if matches!(at(j), Some('e') | Some('E')) {
        let mut k = j + 1;
        let mut exp = String::from("e");
        if matches!(at(k), Some('+') | Some('-')) {
            exp.push(at(k).unwrap());
            k += 1;
        }
        if at(k).is_some_and(|d| d.is_ascii_digit()) {
            while let Some(c) = at(k) {
                if c.is_ascii_digit() {
                    exp.push(c);
                    k += 1;
                } else {
                    break;
                }
            }
            // Don't swallow words like "2em" — require a non-letter after.
            if !at(k).is_some_and(|c| c.is_alphabetic()) {
                s.push_str(&exp);
                j = k;
            }
        }
    }
    (s.parse::<f64>().unwrap_or(0.0), j)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nums(line: &str) -> Vec<f64> {
        lex(line)
            .into_iter()
            .filter_map(|t| match t.tok {
                Tok::Num(v) => Some(v),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn numbers() {
        assert_eq!(nums("1,000,000"), vec![1_000_000.0]);
        assert_eq!(nums("3.5 + .5"), vec![3.5, 0.5]);
        assert_eq!(nums("0xff"), vec![255.0]);
        assert_eq!(nums("5k 2M"), vec![5000.0, 2_000_000.0]);
        assert_eq!(nums("1e3"), vec![1000.0]);
        assert_eq!(nums("f(1, 2)"), vec![1.0, 2.0]);
    }

    #[test]
    fn comments() {
        let t = lex("5 // hello");
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].tok, Tok::Comment);
        assert_eq!(lex("# Heading")[0].tok, Tok::Comment);
    }
}
