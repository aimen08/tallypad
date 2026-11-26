//! Classifies tokens, parses expressions (Pratt parser) and evaluates whole
//! documents line by line.

use std::collections::HashMap;
use std::ops::Range;

use super::lexer::{Tok, Token, lex};
use super::units::{self, UnitId};
use super::value::{EvalResult, Fmt, UnitPow, Value};

/// Syntax-highlight category for a span of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hl {
    Number,
    Unit,
    Operator,
    Keyword,
    Variable,
    Function,
    Comment,
    Label,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Func {
    Sqrt,
    Cbrt,
    Abs,
    Round,
    Floor,
    Ceil,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Log,
    Log2,
    Ln,
    Exp,
}

#[derive(Debug, Clone, PartialEq)]
enum Item {
    Num(f64),
    Unit(UnitId, i8),
    Op(char),
    /// `in` / `to` / `as` / `into`. `true` when the word was literally "in".
    Convert(bool),
    Of,
    Off,
    On,
    Mod,
    Per,
    Scale(f64),
    PowWord(i8),
    Fmt(Fmt),
    Sum,
    Avg,
    Prev,
    Line(usize),
    Var(String),
    Const(f64),
    Func(Func),
}

/// Per-document evaluation state shared across lines.
#[derive(Default)]
struct Scope {
    vars: HashMap<String, Value>,
    /// Variable names split into lower-cased words, longest first.
    var_words: Vec<Vec<String>>,
    results: Vec<Option<Value>>,
    block_start: usize,
}

impl Scope {
    fn define(&mut self, name: &str, value: Value) {
        let key = name.to_lowercase();
        if !self.vars.contains_key(&key) {
            self.var_words
                .push(key.split_whitespace().map(str::to_string).collect());
            self.var_words.sort_by_key(|w| std::cmp::Reverse(w.len()));
        }
        self.vars.insert(key, value);
    }

    fn block_values(&self) -> impl Iterator<Item = &Value> {
        self.results[self.block_start..]
            .iter()
            .filter_map(|r| r.as_ref())
    }

    fn sum(&self) -> Value {
        let mut acc: Option<Value> = None;
        for v in self.block_values() {
            acc = Some(match acc {
                None => v.clone(),
                Some(a) => a.add(v).unwrap_or(a),
            });
        }
        acc.unwrap_or_else(|| Value::num(0.0))
    }

    fn average(&self) -> EvalResult<Value> {
        let count = self.block_values().count();
        if count == 0 {
            return Ok(Value::num(0.0));
        }
        self.sum().div(&Value::num(count as f64))
    }

    fn prev(&self) -> EvalResult<Value> {
        self.results
            .iter()
            .rev()
            .find_map(|r| r.clone())
            .ok_or_else(|| "no previous result".to_string())
    }
}

/// Result of evaluating one line.
#[derive(Debug, Clone, Default)]
pub struct LineOutput {
    pub value: Option<Value>,
    pub display: Option<String>,
    pub spans: Vec<(Range<usize>, Hl)>,
    /// The line refers to other lines or variables (sum, prev, line 2, x).
    pub aggregate: bool,
    /// Variable this line defines ("hourly rate = $45"), as written.
    pub defines: Option<String>,
}

pub fn evaluate_document(text: &str) -> Vec<LineOutput> {
    let mut scope = Scope::default();
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let output = evaluate_line(line, &mut scope);
        let is_break = line.trim().is_empty() || line.trim_start().starts_with('#');
        scope.results.push(output.value.clone());
        if is_break {
            scope.block_start = scope.results.len();
        }
        out.push(output);
    }
    out
}

fn evaluate_line(line: &str, scope: &mut Scope) -> LineOutput {
    let mut tokens = lex(line);
    let mut spans = Vec::new();

    for t in &tokens {
        if matches!(t.tok, Tok::Comment | Tok::Quoted) {
            spans.push((t.span.clone(), Hl::Comment));
        }
    }
    tokens.retain(|t| !matches!(t.tok, Tok::Comment | Tok::Quoted));

    // Labels: "Groceries: $40" -> everything before the colon is a label.
    if let Some(idx) = tokens.iter().position(|t| t.tok == Tok::Op(':')) {
        let between_numbers = idx > 0
            && matches!(tokens[idx - 1].tok, Tok::Num(_))
            && tokens
                .get(idx + 1)
                .is_some_and(|t| matches!(t.tok, Tok::Num(_)));
        if !between_numbers {
            let label_end = tokens[idx].span.end;
            let label_start = tokens.first().map(|t| t.span.start).unwrap_or(0);
            spans.push((label_start..label_end, Hl::Label));
            tokens.drain(..=idx);
        }
    }

    // Assignment: "monthly rent = $1,200"
    let mut assign: Option<String> = None;
    if let Some(eq) = tokens.iter().position(|t| t.tok == Tok::Op('=')) {
        let names: Option<Vec<&str>> = tokens[..eq]
            .iter()
            .map(|t| match &t.tok {
                Tok::Word(w) => Some(w.as_str()),
                _ => None,
            })
            .collect();
        if let Some(names) = names
            && !names.is_empty()
        {
            assign = Some(names.join(" "));
            let span = tokens[0].span.start..tokens[eq - 1].span.end;
            spans.push((span, Hl::Variable));
            spans.push((tokens[eq].span.clone(), Hl::Operator));
            tokens.drain(..=eq);
        }
    }

    let mut items = classify(&tokens, scope, &mut spans);
    spans.sort_by_key(|(r, _)| r.start);

    // Prose before the first value ("Meeting in 5 days") shouldn't block it.
    let first_value = items.iter().position(|it| {
        !matches!(
            it,
            Item::Convert(_)
                | Item::Of
                | Item::Off
                | Item::On
                | Item::Per
                | Item::Mod
                | Item::Scale(_)
                | Item::PowWord(_)
                | Item::Fmt(_)
                | Item::Op('*')
                | Item::Op('/')
                | Item::Op('^')
                | Item::Op('%')
                | Item::Op('!')
                | Item::Op(')')
                | Item::Op(',')
                | Item::Op(':')
                | Item::Op('=')
        )
    });
    match first_value {
        Some(idx) => {
            items.drain(..idx);
        }
        None => items.clear(),
    }

    let mut output = LineOutput {
        spans,
        ..Default::default()
    };
    if items.is_empty() {
        return output;
    }
    output.aggregate = items.iter().any(|it| {
        matches!(
            it,
            Item::Sum | Item::Avg | Item::Prev | Item::Line(_) | Item::Var(_)
        )
    });
    let mut parser = Parser {
        items: &items,
        pos: 0,
        scope,
    };
    let value = parser.parse_expr(0);
    if let Ok(value) = value
        && let Ok(display) = value.format()
    {
        if let Some(name) = assign {
            scope.define(&name, value.clone());
            output.defines = Some(name);
        }
        output.display = Some(display);
        output.value = Some(value);
    }
    output
}

fn keyword(word: &str) -> Option<Item> {
    let w = word.to_lowercase();
    Some(match w.as_str() {
        "in" => Item::Convert(true),
        "to" | "as" | "into" => Item::Convert(false),
        "of" => Item::Of,
        "off" => Item::Off,
        "on" => Item::On,
        "plus" | "add" => Item::Op('+'),
        "minus" | "subtract" | "less" => Item::Op('-'),
        "times" | "multiplied" | "x" => Item::Op('*'),
        "divided" | "over" => Item::Op('/'),
        "per" => Item::Per,
        "mod" | "modulo" | "rem" => Item::Mod,
        "percent" | "pct" => Item::Op('%'),
        "thousand" => Item::Scale(1e3),
        "million" | "mil" => Item::Scale(1e6),
        "billion" => Item::Scale(1e9),
        "trillion" => Item::Scale(1e12),
        "dozen" => Item::Scale(12.0),
        "squared" => Item::PowWord(2),
        "cubed" => Item::PowWord(3),
        "hex" | "hexadecimal" => Item::Fmt(Fmt::Hex),
        "binary" => Item::Fmt(Fmt::Bin),
        "octal" => Item::Fmt(Fmt::Oct),
        "sci" | "scientific" => Item::Fmt(Fmt::Sci),
        "sum" | "total" => Item::Sum,
        "average" | "avg" | "mean" => Item::Avg,
        "prev" | "previous" | "ans" | "answer" => Item::Prev,
        "pi" | "π" => Item::Const(std::f64::consts::PI),
        "tau" => Item::Const(std::f64::consts::TAU),
        "e" => Item::Const(std::f64::consts::E),
        "sqrt" | "root" => Item::Func(Func::Sqrt),
        "cbrt" => Item::Func(Func::Cbrt),
        "abs" => Item::Func(Func::Abs),
        "round" => Item::Func(Func::Round),
        "floor" => Item::Func(Func::Floor),
        "ceil" => Item::Func(Func::Ceil),
        "sin" => Item::Func(Func::Sin),
        "cos" => Item::Func(Func::Cos),
        "tan" => Item::Func(Func::Tan),
        "asin" | "arcsin" => Item::Func(Func::Asin),
        "acos" | "arccos" => Item::Func(Func::Acos),
        "atan" | "arctan" => Item::Func(Func::Atan),
        "sinh" => Item::Func(Func::Sinh),
        "cosh" => Item::Func(Func::Cosh),
        "tanh" => Item::Func(Func::Tanh),
        "log" | "log10" => Item::Func(Func::Log),
        "log2" => Item::Func(Func::Log2),
        "ln" => Item::Func(Func::Ln),
        "exp" => Item::Func(Func::Exp),
        _ => return None,
    })
}

fn hl_for(item: &Item) -> Hl {
    match item {
        Item::Num(_) | Item::Scale(_) | Item::Const(_) => Hl::Number,
        Item::Unit(..) => Hl::Unit,
        Item::Op(_) => Hl::Operator,
        Item::Var(_) | Item::Line(_) => Hl::Variable,
        Item::Func(_) => Hl::Function,
        _ => Hl::Keyword,
    }
}

/// Unit lookup that also understands exponents: `m2`, `cm³`, `km²`.
fn lookup_unit(word: &str) -> Option<(UnitId, i8)> {
    let t = units::table();
    if let Some(id) = t.lookup(word) {
        return Some((id, 1));
    }
    let (base, power) = if let Some(b) = word.strip_suffix('²') {
        (b, 2)
    } else if let Some(b) = word.strip_suffix('³') {
        (b, 3)
    } else if let Some(b) = word.strip_suffix('2') {
        (b, 2)
    } else if let Some(b) = word.strip_suffix('3') {
        (b, 3)
    } else {
        return None;
    };
    let id = t.lookup(base)?;
    let u = t.get(id);
    (u.offset == 0.0 && !u.is_currency()).then_some((id, power))
}

fn classify(tokens: &[Token], scope: &Scope, spans: &mut Vec<(Range<usize>, Hl)>) -> Vec<Item> {
    let mut items = Vec::new();
    let t = units::table();
    let mut i = 0;
    let word_at = |k: usize| match tokens.get(k).map(|t| &t.tok) {
        Some(Tok::Word(w)) => Some(w.to_lowercase()),
        _ => None,
    };

    while i < tokens.len() {
        let tok = &tokens[i];
        let mut push = |item: Item, span: Range<usize>, items: &mut Vec<Item>| {
            spans.push((span, hl_for(&item)));
            items.push(item);
        };
        match &tok.tok {
            Tok::Num(v) => push(Item::Num(*v), tok.span.clone(), &mut items),
            Tok::Currency(code) => {
                if let Some(id) = t.currency(code) {
                    push(Item::Unit(id, 1), tok.span.clone(), &mut items);
                }
            }
            Tok::Op(c) => push(Item::Op(*c), tok.span.clone(), &mut items),
            Tok::Word(word) => {
                // Multi-word variables, longest match first.
                let mut matched = false;
                for words in &scope.var_words {
                    let fits = words
                        .iter()
                        .enumerate()
                        .all(|(k, w)| word_at(i + k).as_deref() == Some(w.as_str()));
                    if fits {
                        let span = tok.span.start..tokens[i + words.len() - 1].span.end;
                        push(Item::Var(words.join(" ")), span, &mut items);
                        i += words.len();
                        matched = true;
                        break;
                    }
                }
                if matched {
                    continue;
                }
                let lower = word.to_lowercase();
                // "line 3" / "line3"
                if lower == "line"
                    && let Some(Tok::Num(n)) = tokens.get(i + 1).map(|t| &t.tok)
                {
                    let span = tok.span.start..tokens[i + 1].span.end;
                    push(Item::Line(*n as usize), span, &mut items);
                    i += 2;
                    continue;
                }
                if let Some(n) = lower
                    .strip_prefix("line")
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    push(Item::Line(n), tok.span.clone(), &mut items);
                    i += 1;
                    continue;
                }
                // Two-word units: "fl oz", "light years", "sq ft"
                if let Some(next) = word_at(i + 1)
                    && let Some(id) = t.lookup(&format!("{lower} {next}"))
                {
                    let span = tok.span.start..tokens[i + 1].span.end;
                    push(Item::Unit(id, 1), span, &mut items);
                    i += 2;
                    continue;
                }
                // "multiplied by", "divided by"
                if matches!(lower.as_str(), "multiplied" | "divided")
                    && word_at(i + 1).as_deref() == Some("by")
                {
                    let op = if lower == "multiplied" { '*' } else { '/' };
                    let span = tok.span.start..tokens[i + 1].span.end;
                    push(Item::Op(op), span, &mut items);
                    i += 2;
                    continue;
                }
                // "e" and "x" are only keywords when they aren't units/vars.
                if let Some(kw) = keyword(word) {
                    push(kw, tok.span.clone(), &mut items);
                } else if let Some((id, p)) = lookup_unit(word) {
                    push(Item::Unit(id, p), tok.span.clone(), &mut items);
                }
                // Unknown words are ignored (plain text).
            }
            Tok::Comment | Tok::Quoted => {}
        }
        i += 1;
    }
    items
}

struct Parser<'a> {
    items: &'a [Item],
    pos: usize,
    scope: &'a Scope,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Item> {
        self.items.get(self.pos)
    }

    fn peek_at(&self, offset: usize) -> Option<&Item> {
        self.items.get(self.pos + offset)
    }

    fn next(&mut self) -> Option<Item> {
        let item = self.items.get(self.pos).cloned();
        self.pos += 1;
        item
    }

    fn is_unit_at(&self, offset: usize) -> bool {
        matches!(self.peek_at(offset), Some(Item::Unit(..)))
    }

    /// Whether the item after a `Convert` makes it a conversion.
    fn is_conversion_target(&self, offset: usize) -> bool {
        matches!(
            self.peek_at(offset),
            Some(Item::Unit(..)) | Some(Item::Fmt(_)) | Some(Item::Op('%'))
        )
    }

    /// unit_expr := atom (('/' | '*' | per) atom)*
    fn parse_units(&mut self) -> Option<Vec<UnitPow>> {
        let mut out: Vec<UnitPow> = Vec::new();
        let mut sign = 1i8;
        loop {
            let Some(Item::Unit(id, mut p)) = self.peek().cloned() else {
                break;
            };
            self.pos += 1;
            if self.peek() == Some(&Item::Op('^'))
                && let Some(Item::Num(n)) = self.peek_at(1)
                && n.fract() == 0.0
            {
                p *= *n as i8;
                self.pos += 2;
            }
            match out.iter_mut().find(|(i, _)| *i == id) {
                Some(e) => e.1 += p * sign,
                None => out.push((id, p * sign)),
            }
            match self.peek() {
                Some(Item::Op('/')) | Some(Item::Per) if self.is_unit_at(1) => {
                    sign = -1;
                    self.pos += 1;
                }
                Some(Item::Op('*')) if self.is_unit_at(1) => {
                    sign = 1;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        out.retain(|(_, p)| *p != 0);
        (!out.is_empty()).then_some(out)
    }

    /// Attaches trailing units to a number: `5 km`, `60 km/h`, `3 in`.
    fn attach_units(&mut self, value: Value) -> EvalResult<Value> {
        // A bare "in" after a number means inches unless it starts a conversion.
        if self.peek() == Some(&Item::Convert(true)) && !self.is_conversion_target(1) {
            self.pos += 1;
            let inch = units::table().lookup("in").unwrap();
            return value.mul(&Value::from_units(1.0, vec![(inch, 1)]));
        }
        if let Some(units_list) = self.parse_units() {
            if value.is_plain() {
                let mut v = Value::from_units(value.v, units_list);
                v.fmt = value.fmt;
                return Ok(v);
            }
            return value.mul(&Value::from_units(1.0, units_list));
        }
        Ok(value)
    }

    fn parse_expr(&mut self, min_bp: u8) -> EvalResult<Value> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let Some(item) = self.peek().cloned() else {
                break;
            };
            // Postfix operators
            match item {
                Item::Op('%') => {
                    if 50 < min_bp {
                        break;
                    }
                    self.pos += 1;
                    if lhs.percent || !lhs.is_plain() {
                        return Err("bad percentage".into());
                    }
                    lhs = Value::percent(lhs.v);
                    continue;
                }
                Item::Op('!') => {
                    if 50 < min_bp {
                        break;
                    }
                    self.pos += 1;
                    lhs = factorial(&lhs)?;
                    continue;
                }
                Item::PowWord(p) => {
                    if 50 < min_bp {
                        break;
                    }
                    self.pos += 1;
                    lhs = lhs.pow(&Value::num(p as f64))?;
                    continue;
                }
                Item::Scale(s) => {
                    if 50 < min_bp {
                        break;
                    }
                    self.pos += 1;
                    lhs = lhs.mul(&Value::num(s))?;
                    continue;
                }
                Item::Unit(..) => {
                    if 45 < min_bp {
                        break;
                    }
                    lhs = self.attach_units(lhs)?;
                    continue;
                }
                _ => {}
            }

            let (l_bp, r_bp) = match &item {
                Item::Convert(_) => (1, 2),
                Item::Op('+') | Item::Op('-') => (10, 11),
                Item::Op('*') | Item::Op('/') | Item::Mod | Item::Per => (20, 21),
                Item::Of | Item::Off | Item::On => (20, 21),
                Item::Op('^') => (41, 40),
                // Implicit multiplication: 2 pi, 3(4 + 1), 2 x
                Item::Op('(') | Item::Var(_) | Item::Const(_) | Item::Func(_) => (20, 21),
                _ => break,
            };
            if l_bp < min_bp {
                break;
            }

            if let Item::Convert(is_in) = item {
                if !self.is_conversion_target(1) {
                    if is_in && lhs.is_plain() {
                        // "5 in" -> inches
                        lhs = self.attach_units(lhs)?;
                        continue;
                    }
                    break;
                }
                self.pos += 1;
                lhs = self.parse_conversion(lhs)?;
                continue;
            }

            let implicit = matches!(
                item,
                Item::Op('(') | Item::Var(_) | Item::Const(_) | Item::Func(_)
            );
            if !implicit {
                self.pos += 1;
            }
            let rhs = self.parse_expr(r_bp)?;
            lhs = match item {
                Item::Op('+') => lhs.add(&rhs)?,
                Item::Op('-') => lhs.sub(&rhs)?,
                Item::Op('*') => lhs.mul(&rhs)?,
                Item::Op('/') | Item::Per => lhs.div(&rhs)?,
                Item::Mod => lhs.rem(&rhs)?,
                Item::Op('^') => lhs.pow(&rhs)?,
                Item::Of => lhs.mul(&rhs)?,
                Item::Off if lhs.percent => rhs.mul(&Value::num(1.0 - lhs.v / 100.0))?,
                Item::On if lhs.percent => rhs.mul(&Value::num(1.0 + lhs.v / 100.0))?,
                Item::Off | Item::On => return Err("expected a percentage".into()),
                _ => lhs.mul(&rhs)?,
            };
        }
        Ok(lhs)
    }

    fn parse_conversion(&mut self, lhs: Value) -> EvalResult<Value> {
        match self.peek().cloned() {
            Some(Item::Fmt(f)) => {
                self.pos += 1;
                if f != Fmt::Sci && !lhs.is_plain() {
                    return Err("only plain numbers can be shown in that base".into());
                }
                Ok(Value { fmt: f, ..lhs })
            }
            Some(Item::Op('%')) => {
                self.pos += 1;
                // "5 as % of 20" -> 25%
                if self.peek() == Some(&Item::Of) {
                    self.pos += 1;
                    let base = self.parse_expr(2)?;
                    let ratio = lhs.div(&base)?;
                    if !ratio.is_plain() {
                        return Err("incompatible units".into());
                    }
                    return Ok(Value::percent(ratio.v * 100.0));
                }
                if lhs.is_plain() {
                    return Ok(Value::percent(lhs.v * 100.0));
                }
                Err("can't express as a percentage".into())
            }
            Some(Item::Unit(..)) => {
                let target = self.parse_units().ok_or("expected unit")?;
                lhs.convert(target)
            }
            _ => Err("expected conversion target".into()),
        }
    }

    fn parse_prefix(&mut self) -> EvalResult<Value> {
        let item = self.next().ok_or("unexpected end")?;
        match item {
            Item::Num(n) => {
                let mut v = Value::num(n);
                while let Some(Item::Scale(s)) = self.peek() {
                    v = Value::num(v.v * s);
                    self.pos += 1;
                }
                self.attach_units(v)
            }
            Item::Unit(id, p) => {
                let u = units::table().get(id);
                let amount_follows = matches!(
                    self.peek(),
                    Some(Item::Num(_))
                        | Some(Item::Op('('))
                        | Some(Item::Op('-'))
                        | Some(Item::Var(_))
                        | Some(Item::Const(_))
                        | Some(Item::Func(_))
                        | Some(Item::Sum)
                        | Some(Item::Avg)
                        | Some(Item::Prev)
                        | Some(Item::Line(_))
                );
                if u.is_currency() && p == 1 && amount_follows {
                    // $5, € 20, USD 30
                    let amount = self.parse_expr(30)?;
                    if amount.is_plain() {
                        return Ok(Value::from_units(amount.v, vec![(id, 1)]));
                    }
                    // "$ sum" where sum already carries a currency
                    return amount.convert(vec![(id, 1)]);
                }
                self.pos -= 1;
                let after_divide =
                    self.pos > 0 && matches!(self.items[self.pos - 1], Item::Op('/') | Item::Per);
                let list = self.parse_units().ok_or("expected unit")?;
                if after_divide || matches!(self.peek(), Some(Item::Convert(_))) || u.is_currency()
                {
                    // A bare unit means one of it: "km in m", "USD in EUR"
                    return Ok(Value::from_units(1.0, list));
                }
                // A stray unit word in prose ("I'm", "a t-shirt"): skip it.
                self.parse_prefix()
            }
            Item::Op('-') => Ok(self.parse_expr(30)?.neg()),
            Item::Op('+') => self.parse_expr(30),
            Item::Op('(') => {
                let v = self.parse_expr(0)?;
                if self.peek() == Some(&Item::Op(')')) {
                    self.pos += 1;
                }
                Ok(v)
            }
            Item::Var(name) => self
                .scope
                .vars
                .get(&name)
                .cloned()
                .ok_or_else(|| "unknown variable".into()),
            Item::Const(c) => Ok(Value::num(c)),
            Item::Func(f) => {
                // sqrt 16, sqrt(16), sqrt of 16
                if self.peek() == Some(&Item::Of) {
                    self.pos += 1;
                }
                let arg = self.parse_expr(35)?;
                apply_func(f, &arg)
            }
            Item::Sum => Ok(self.scope.sum()),
            Item::Avg => self.scope.average(),
            Item::Prev => self.scope.prev(),
            Item::Line(n) => self
                .scope
                .results
                .get(n.wrapping_sub(1))
                .cloned()
                .flatten()
                .ok_or_else(|| "no such line".into()),
            _ => Err("unexpected token".into()),
        }
    }
}

fn factorial(v: &Value) -> EvalResult<Value> {
    if !v.is_plain() || v.v < 0.0 || v.v.fract() != 0.0 || v.v > 170.0 {
        return Err("factorial needs a small whole number".into());
    }
    Ok(Value::num((1..=v.v as u64).map(|k| k as f64).product()))
}

fn apply_func(f: Func, arg: &Value) -> EvalResult<Value> {
    use Func::*;
    let angle_dims = units::dim(units::ANGLE, 1);
    let x = match f {
        Sin | Cos | Tan => {
            if arg.dims == angle_dims {
                arg.v
            } else if arg.is_plain() {
                arg.v
            } else {
                return Err("expected an angle".into());
            }
        }
        Abs | Round | Floor | Ceil => {
            let shown = arg.shown();
            let r = match f {
                Abs => shown.abs(),
                Round => shown.round(),
                Floor => shown.floor(),
                _ => shown.ceil(),
            };
            let mut out = arg.clone();
            out.v = if arg.percent { r } else { arg.from_display(r) };
            return Ok(out);
        }
        Sqrt | Cbrt if !arg.is_plain() => {
            let p = if f == Sqrt { 0.5 } else { 1.0 / 3.0 };
            let all_even = arg.dims.iter().all(|d| (*d as f64 * p).fract() == 0.0)
                && arg
                    .display
                    .iter()
                    .all(|(_, q)| (*q as f64 * p).fract() == 0.0);
            if !all_even {
                return Err("can't take that root of these units".into());
            }
            let mut out = arg.clone();
            out.v = arg.v.powf(p);
            for d in out.dims.iter_mut() {
                *d = (*d as f64 * p) as i8;
            }
            for (_, q) in out.display.iter_mut() {
                *q = (*q as f64 * p) as i8;
            }
            return Ok(out);
        }
        _ => {
            if !arg.is_plain() {
                return Err("expected a plain number".into());
            }
            arg.v
        }
    };
    let r = match f {
        Sqrt => x.sqrt(),
        Cbrt => x.cbrt(),
        Sin => x.sin(),
        Cos => x.cos(),
        Tan => x.tan(),
        Asin => x.asin(),
        Acos => x.acos(),
        Atan => x.atan(),
        Sinh => x.sinh(),
        Cosh => x.cosh(),
        Tanh => x.tanh(),
        Log => x.log10(),
        Log2 => x.log2(),
        Ln => x.ln(),
        Exp => x.exp(),
        Abs | Round | Floor | Ceil => unreachable!(),
    };
    if !r.is_finite() {
        return Err("out of range".into());
    }
    // Round away float noise like sin(pi) = 1.2e-16
    let r = if r.abs() < 1e-12 { 0.0 } else { r };
    Ok(Value::num(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(doc: &str) -> Vec<Option<String>> {
        evaluate_document(doc)
            .into_iter()
            .map(|l| l.display)
            .collect()
    }

    fn one(line: &str) -> String {
        eval(line)[0].clone().unwrap_or_else(|| "<none>".into())
    }

    #[test]
    fn arithmetic() {
        assert_eq!(one("2 + 3 * 4"), "14");
        assert_eq!(one("(2 + 3) * 4"), "20");
        assert_eq!(one("2 ^ 10"), "1,024");
        assert_eq!(one("10 / 4"), "2.5");
        assert_eq!(one("-3 + 5"), "2");
        assert_eq!(one("5!"), "120");
        assert_eq!(one("10 mod 3"), "1");
        assert_eq!(one("2 pi"), "6.2832");
        assert_eq!(one("3 times 4"), "12");
        assert_eq!(one("12 divided by 4"), "3");
        assert_eq!(one("1,500 + 500"), "2,000");
        assert_eq!(one("5k + 1"), "5,001");
        assert_eq!(one("2 million / 4"), "500,000");
        assert_eq!(one("3 squared"), "9");
    }

    #[test]
    fn fuzzy_text() {
        assert_eq!(one("$20 for lunch + $15 for dinner"), "$35.00");
        assert_eq!(one("Groceries: $45.50"), "$45.50");
        assert_eq!(one("5 apples * 3"), "15");
        assert_eq!(eval("just some text")[0], None);
        assert_eq!(eval("# Heading 2024")[0], None);
        assert_eq!(one("10 // a comment 5"), "10");
    }

    #[test]
    fn percentages() {
        assert_eq!(one("20% of 50"), "10");
        assert_eq!(one("50 + 10%"), "55");
        assert_eq!(one("$100 - 15%"), "$85.00");
        assert_eq!(one("10% off $200"), "$180.00");
        assert_eq!(one("5% on 200"), "210");
        assert_eq!(one("5 as % of 20"), "25%");
        assert_eq!(one("0.35 as %"), "35%");
        assert_eq!(one("20%"), "20%");
    }

    #[test]
    fn units_and_conversion() {
        assert_eq!(one("5 km in m"), "5,000 m");
        assert_eq!(one("1 mile in km"), "1.6093 km");
        assert_eq!(one("3 ft + 12 in"), "4 ft");
        assert_eq!(one("100 °C in F"), "212 °F");
        assert_eq!(one("20 C + 5"), "25 °C");
        assert_eq!(one("60 km/h * 2 h"), "120 km");
        assert_eq!(one("10 km / 2 h"), "5 km/h");
        assert_eq!(one("100 km/h to mph"), "62.1371 mph");
        assert_eq!(one("3 m * 4 m"), "12 m²");
        assert_eq!(one("2 GB in MB"), "2,000 MB");
        assert_eq!(one("90 min in hours"), "1.5 h");
        assert_eq!(one("5 lb to kg"), "2.268 kg");
        assert_eq!(one("1 gallon in liters"), "3.7854 l");
        assert_eq!(one("12 in"), "12 in");
        assert_eq!(one("255 in hex"), "0xFF");
        assert_eq!(one("0xFF + 1"), "256");
        assert_eq!(one("2 m2 in ft2"), "21.5278 ft²");
        assert_eq!(one("60 km/h * 45 min"), "45 km");
        assert_eq!(one("100 mph * 2 h"), "200 mi");
        assert_eq!(one("Meeting in 5 days"), "5 days");
        assert_eq!(one("I'm paying $30"), "$30.00");
        assert_eq!(one("USD in EUR"), "€0.86");
    }

    #[test]
    fn currency() {
        assert_eq!(one("$10 + $5"), "$15.00");
        assert_eq!(one("€10 in USD"), "$11.60");
        assert_eq!(one("100 EUR to GBP"), "£87.22");
        assert_eq!(one("$25/h * 8 h"), "$200.00");
        assert_eq!(one("50 CAD"), "50.00 CAD");
    }

    #[test]
    fn variables_and_references() {
        let r = eval(
            "rent = $1,200\nutilities = $150\nrent + utilities\nmonthly cost = prev\nmonthly cost * 12",
        );
        assert_eq!(r[0].as_deref(), Some("$1,200.00"));
        assert_eq!(r[2].as_deref(), Some("$1,350.00"));
        assert_eq!(r[3].as_deref(), Some("$1,350.00"));
        assert_eq!(r[4].as_deref(), Some("$16,200.00"));

        let r = eval("10\n20\n30\nsum\n\n5\n5\ntotal\naverage");
        assert_eq!(r[3].as_deref(), Some("60"));
        assert_eq!(r[7].as_deref(), Some("10"));

        let r = eval("4\n6\nline1 * line 2");
        assert_eq!(r[2].as_deref(), Some("24"));

        let r = eval("x = 3\n2x + 1");
        assert_eq!(r[1].as_deref(), Some("7"));

        let out = evaluate_document("Hourly Rate = $45\nhourly rate * 2\nbad = foo");
        assert_eq!(out[0].defines.as_deref(), Some("Hourly Rate"));
        assert_eq!(out[1].defines, None);
        assert_eq!(out[2].defines, None);
    }

    #[test]
    fn functions() {
        assert_eq!(one("sqrt 16"), "4");
        assert_eq!(one("sqrt(16) + 1"), "5");
        assert_eq!(one("sin(90 deg)"), "1");
        assert_eq!(one("round 3.6"), "4");
        assert_eq!(one("log 1000"), "3");
        assert_eq!(one("sqrt(16 m2)"), "4 m");
    }

    #[test]
    fn welcome_sheet() {
        let doc = include_str!("../welcome.txt");
        let results = eval(doc);
        let mut report = String::new();
        for (line, r) in doc.lines().zip(&results) {
            report.push_str(&format!("{line:<40} => {r:?}\n"));
            let is_prose = line.trim().is_empty()
                || line.starts_with('#')
                || line.starts_with("//")
                || line.starts_with("Write");
            assert_eq!(r.is_some(), !is_prose, "line {line:?}\n{report}");
        }
        println!("{report}");
    }

    #[test]
    fn highlight_spans() {
        let doc = evaluate_document("rent = $5 // note");
        let kinds: Vec<Hl> = doc[0].spans.iter().map(|(_, h)| *h).collect();
        assert_eq!(
            kinds,
            vec![
                Hl::Variable,
                Hl::Operator,
                Hl::Unit,
                Hl::Number,
                Hl::Comment
            ]
        );
    }
}
