//! Values: numbers with dimensions, display units and percentages.

use super::units::{self, Dims, NO_DIMS, UnitId};

pub type EvalResult<T> = Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    Default,
    Hex,
    Bin,
    Oct,
    Sci,
}

/// A unit with an integer power, e.g. `h^-1`.
pub type UnitPow = (UnitId, i8);

#[derive(Debug, Clone)]
pub struct Value {
    /// Magnitude in SI base units (or the percent number when `percent`).
    pub v: f64,
    pub dims: Dims,
    /// The units this value should be shown in.
    pub display: Vec<UnitPow>,
    pub percent: bool,
    pub fmt: Fmt,
}

impl Value {
    pub fn num(v: f64) -> Self {
        Value {
            v,
            dims: NO_DIMS,
            display: Vec::new(),
            percent: false,
            fmt: Fmt::Default,
        }
    }

    pub fn percent(p: f64) -> Self {
        Value {
            percent: true,
            ..Value::num(p)
        }
    }

    pub fn is_plain(&self) -> bool {
        !self.percent && self.dims == NO_DIMS && self.display.is_empty()
    }

    fn with_v(&self, v: f64) -> Self {
        Value { v, ..self.clone() }
    }

    /// Builds a value from a number expressed in `display` units.
    pub fn from_units(n: f64, display: Vec<UnitPow>) -> Self {
        let dims = dims_of(&display);
        let mut val = Value {
            v: 0.0,
            dims,
            display,
            percent: false,
            fmt: Fmt::Default,
        };
        val.v = val.from_display(n);
        if val.dims == NO_DIMS {
            val.display.clear();
        }
        val
    }

    /// The single offset unit (temperature), if the display is exactly one.
    fn offset_unit(&self) -> Option<UnitId> {
        if let [(id, 1)] = self.display.as_slice() {
            let u = units::table().get(*id);
            if u.offset != 0.0 {
                return Some(*id);
            }
        }
        None
    }

    fn display_factor(&self) -> f64 {
        let t = units::table();
        self.display
            .iter()
            .map(|(id, p)| t.factor(*id).powi(*p as i32))
            .product()
    }

    /// SI magnitude -> number in display units.
    pub fn to_display(&self, si: f64) -> f64 {
        if let Some(id) = self.offset_unit() {
            let u = units::table().get(id);
            return si / u.factor - u.offset;
        }
        if self.display.is_empty() {
            return si;
        }
        si / self.display_factor()
    }

    /// Number in display units -> SI magnitude.
    pub fn from_display(&self, n: f64) -> f64 {
        if let Some(id) = self.offset_unit() {
            let u = units::table().get(id);
            return (n + u.offset) * u.factor;
        }
        if self.display.is_empty() {
            return n;
        }
        n * self.display_factor()
    }

    pub fn shown(&self) -> f64 {
        if self.percent {
            self.v
        } else {
            self.to_display(self.v)
        }
    }

    /// Applies `f` in display space (used for scalar ops on offset units).
    fn map_display(&self, f: impl Fn(f64) -> f64) -> Value {
        let shown = self.to_display(self.v);
        self.with_v(self.from_display(f(shown)))
    }

    pub fn neg(&self) -> Value {
        if self.percent {
            return self.with_v(-self.v);
        }
        self.map_display(|x| -x)
    }

    pub fn add(&self, other: &Value) -> EvalResult<Value> {
        self.add_signed(other, 1.0)
    }

    pub fn sub(&self, other: &Value) -> EvalResult<Value> {
        self.add_signed(other, -1.0)
    }

    fn add_signed(&self, b: &Value, sign: f64) -> EvalResult<Value> {
        let a = self;
        match (a.percent, b.percent) {
            (true, true) => return Ok(a.with_v(a.v + sign * b.v)),
            // 50 + 10%  ->  55
            (false, true) => return Ok(a.with_v(a.v * (1.0 + sign * b.v / 100.0))),
            // 10% + 50  ->  55
            (true, false) => {
                let base = if sign < 0.0 { b.neg() } else { b.clone() };
                return Ok(base.with_v(base.v * (1.0 + a.v / 100.0)));
            }
            _ => {}
        }
        if a.is_plain() && !b.is_plain() {
            // 3 + 5 km -> 8 km
            let n = a.v + sign * b.to_display(b.v);
            let mut r = b.with_v(b.from_display(n));
            r.fmt = a.fmt;
            return Ok(r);
        }
        if b.is_plain() && !a.is_plain() {
            let n = a.to_display(a.v) + sign * b.v;
            return Ok(a.with_v(a.from_display(n)));
        }
        if a.dims != b.dims {
            return Err("incompatible units".into());
        }
        if a.display.is_empty() {
            let mut r = b.with_v(a.v + sign * b.v);
            r.fmt = a.fmt;
            return Ok(r);
        }
        // Add in the left operand's display space so offset units
        // (20 °C + 5 °C = 25 °C) behave the way people expect.
        let n = a.to_display(a.v) + sign * a.to_display(b.v);
        Ok(a.with_v(a.from_display(n)))
    }

    pub fn mul(&self, b: &Value) -> EvalResult<Value> {
        let a = self;
        match (a.percent, b.percent) {
            (true, true) => return Ok(Value::percent(a.v * b.v / 100.0)),
            (false, true) => return Ok(a.with_v(a.v * b.v / 100.0)),
            (true, false) => return Ok(b.with_v(b.v * a.v / 100.0)),
            _ => {}
        }
        if b.is_plain() {
            let mut r = a.map_display(|x| x * b.v);
            if r.fmt == Fmt::Default {
                r.fmt = b.fmt;
            }
            return Ok(r);
        }
        if a.is_plain() {
            let mut r = b.map_display(|x| x * a.v);
            r.fmt = a.fmt;
            return Ok(r);
        }
        let dims = add_dims(&a.dims, &b.dims, 1);
        let display = if dims == NO_DIMS {
            Vec::new()
        } else {
            merge_units(&a.display, &b.display, 1)
        };
        Ok(Value {
            v: a.v * b.v,
            dims,
            display,
            percent: false,
            fmt: a.fmt,
        })
    }

    pub fn div(&self, b: &Value) -> EvalResult<Value> {
        let a = self;
        let divisor = if b.percent { b.v / 100.0 } else { b.v };
        if divisor == 0.0 {
            return Err("division by zero".into());
        }
        if a.percent && !b.percent {
            return Ok(Value::num(a.v / 100.0 / b.v));
        }
        if b.percent || b.is_plain() {
            return Ok(a.map_display(|x| x / divisor));
        }
        let dims = add_dims(&a.dims, &b.dims, -1);
        let display = if dims == NO_DIMS {
            Vec::new()
        } else {
            merge_units(&a.display, &b.display, -1)
        };
        Ok(Value {
            v: a.v / b.v,
            dims,
            display,
            percent: false,
            fmt: a.fmt,
        })
    }

    pub fn rem(&self, b: &Value) -> EvalResult<Value> {
        if b.v == 0.0 {
            return Err("division by zero".into());
        }
        if b.is_plain() {
            return Ok(self.map_display(|x| x % b.v));
        }
        if self.dims != b.dims {
            return Err("incompatible units".into());
        }
        Ok(self.with_v(self.v % b.v))
    }

    pub fn pow(&self, b: &Value) -> EvalResult<Value> {
        if !b.is_plain() {
            return Err("exponent must be a plain number".into());
        }
        if self.is_plain() || self.percent {
            return Ok(self.with_v(self.v.powf(b.v)));
        }
        if b.v.fract() != 0.0 || b.v.abs() > 12.0 {
            return Err("unit powers must be integers".into());
        }
        let p = b.v as i8;
        Ok(Value {
            v: self.v.powi(p as i32),
            dims: add_dims(&NO_DIMS, &self.dims, p),
            display: self.display.iter().map(|(id, q)| (*id, q * p)).collect(),
            percent: false,
            fmt: self.fmt,
        })
    }

    /// Converts to the given display units.
    pub fn convert(&self, target: Vec<UnitPow>) -> EvalResult<Value> {
        let tdims = dims_of(&target);
        if self.percent {
            return Err("can't convert a percentage".into());
        }
        if self.is_plain() {
            return Ok(Value::from_units(self.v, target));
        }
        if tdims != self.dims {
            return Err("incompatible units".into());
        }
        Ok(Value {
            display: target,
            fmt: Fmt::Default,
            ..self.clone()
        })
    }

    pub fn format(&self) -> EvalResult<String> {
        if !self.v.is_finite() {
            return Err("not a finite number".into());
        }
        if self.percent {
            return Ok(format!("{}%", fmt_number(self.v, None)));
        }
        let mut display = self.display.clone();
        if display.is_empty() && self.dims != NO_DIMS {
            if let Some(id) = units::preferred_unit_for(&self.dims) {
                display = vec![(id, 1)];
            } else {
                return Ok(format!(
                    "{} {}",
                    fmt_number(self.v, None),
                    si_string(&self.dims)
                ));
            }
        }
        let shown_val = Value {
            display: display.clone(),
            ..self.clone()
        };
        let n = shown_val.to_display(self.v);
        if display.is_empty() {
            return Ok(match self.fmt {
                Fmt::Default => fmt_number(n, None),
                f => fmt_radix(n, f),
            });
        }
        let t = units::table();
        if let [(id, 1)] = display.as_slice()
            && let Some(code) = &t.get(*id).currency
        {
            let body = fmt_number(n.abs(), Some(2));
            let sign = if n < 0.0 && body.chars().any(|c| c.is_ascii_digit() && c != '0') {
                "-"
            } else {
                ""
            };
            return Ok(match units::currency_prefix(code) {
                Some(sym) => format!("{sign}{sym}{body}"),
                None => format!("{sign}{body} {code}"),
            });
        }
        let num = fmt_number(n, None);
        let mut unit = unit_string(&display);
        let pluralizable = ["day", "week", "month", "year", "cup", "acre", "bit"];
        if display.len() == 1 && pluralizable.contains(&unit.as_str()) && num != "1" {
            unit.push('s');
        }
        Ok(format!("{num} {unit}"))
    }
}

pub fn dims_of(units_list: &[UnitPow]) -> Dims {
    let t = units::table();
    let mut d = NO_DIMS;
    for (id, p) in units_list {
        d = add_dims(&d, &t.get(*id).dims, *p);
    }
    d
}

fn add_dims(a: &Dims, b: &Dims, scale: i8) -> Dims {
    let mut r = *a;
    for i in 0..r.len() {
        r[i] += b[i] * scale;
    }
    r
}

fn merge_units(a: &[UnitPow], b: &[UnitPow], sign: i8) -> Vec<UnitPow> {
    let t = units::table();
    let expand = |list: &[UnitPow], sign: i8| -> Vec<UnitPow> {
        let mut out = Vec::new();
        for (id, p) in list {
            let u = t.get(*id);
            if u.expand.is_empty() {
                out.push((*id, p * sign));
            } else {
                out.extend(u.expand.iter().map(|(e, q)| (*e, q * p * sign)));
            }
        }
        out
    };
    let mut out: Vec<UnitPow> = Vec::new();
    for (id, p) in expand(a, 1).into_iter().chain(expand(b, sign)) {
        // Units of the same kind collapse into the first one seen, so
        // km/h * min reads as km rather than km·min/h.
        let dims = t.get(id).dims;
        match out.iter_mut().find(|(i, _)| t.get(*i).dims == dims) {
            Some(e) => e.1 += p,
            None => out.push((id, p)),
        }
    }
    out.retain(|(_, p)| *p != 0);
    out
}

fn superscript(p: i8) -> String {
    let digits = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    let mut s = String::new();
    if p < 0 {
        s.push('⁻');
    }
    for c in p.unsigned_abs().to_string().chars() {
        s.push(digits[c.to_digit(10).unwrap() as usize]);
    }
    s
}

pub fn unit_string(display: &[UnitPow]) -> String {
    let t = units::table();
    let fmt_part = |id: UnitId, p: i8| {
        let sym = &t.get(id).symbol;
        if p == 1 {
            sym.clone()
        } else {
            format!("{sym}{}", superscript(p))
        }
    };
    let num: Vec<String> = display
        .iter()
        .filter(|(_, p)| *p > 0)
        .map(|(id, p)| fmt_part(*id, *p))
        .collect();
    let den: Vec<String> = display
        .iter()
        .filter(|(_, p)| *p < 0)
        .map(|(id, p)| fmt_part(*id, -*p))
        .collect();
    match (num.is_empty(), den.is_empty()) {
        (_, true) => num.join("·"),
        (true, false) => format!("1/{}", den.join("·")),
        (false, false) => format!("{}/{}", num.join("·"), den.join("·")),
    }
}

fn si_string(dims: &Dims) -> String {
    let names = ["m", "kg", "s", "B", "K", "USD", "rad"];
    let mut parts = Vec::new();
    for (i, p) in dims.iter().enumerate() {
        if *p == 1 {
            parts.push(names[i].to_string());
        } else if *p != 0 {
            parts.push(format!("{}{}", names[i], superscript(*p)));
        }
    }
    parts.join("·")
}

fn fmt_radix(n: f64, f: Fmt) -> String {
    if f == Fmt::Sci {
        return fmt_sci(n);
    }
    if n.fract() != 0.0 || n.abs() > 9.0e15 {
        return fmt_number(n, None);
    }
    let i = n as i64;
    let (sign, u) = if i < 0 {
        ("-", i.unsigned_abs())
    } else {
        ("", i as u64)
    };
    match f {
        Fmt::Hex => format!("{sign}0x{u:X}"),
        Fmt::Bin => format!("{sign}0b{u:b}"),
        Fmt::Oct => format!("{sign}0o{u:o}"),
        _ => fmt_number(n, None),
    }
}

fn fmt_sci(n: f64) -> String {
    let s = format!("{n:.6e}");
    // Trim mantissa zeros: 1.230000e5 -> 1.23e5
    if let Some((mant, exp)) = s.split_once('e') {
        let mant = if mant.contains('.') {
            mant.trim_end_matches('0').trim_end_matches('.')
        } else {
            mant
        };
        return format!("{mant}e{exp}");
    }
    s
}

/// Formats with thousands separators. `fixed` forces a decimal count.
pub fn fmt_number(n: f64, fixed: Option<usize>) -> String {
    if n == 0.0 {
        return match fixed {
            Some(d) if d > 0 => format!("0.{}", "0".repeat(d)),
            _ => "0".into(),
        };
    }
    let abs = n.abs();
    if fixed.is_none() && (abs >= 1e15 || abs < 1e-9) {
        return fmt_sci(n);
    }
    let decimals = match fixed {
        Some(d) => d,
        None => {
            if abs >= 1.0 {
                4
            } else {
                // Keep ~4 significant digits for small numbers.
                let lead = (-abs.log10()).floor() as usize;
                (lead + 4).min(12)
            }
        }
    };
    let s = format!("{abs:.decimals$}");
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (s.clone(), String::new()),
    };
    let frac = if fixed.is_some() {
        frac_part
    } else {
        frac_part.trim_end_matches('0').to_string()
    };
    let mut grouped = String::new();
    let len = int_part.len();
    for (idx, c) in int_part.chars().enumerate() {
        if idx > 0 && (len - idx) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let is_zero = grouped.chars().all(|c| c == '0' || c == ',') && frac.chars().all(|c| c == '0');
    let sign = if n < 0.0 && !is_zero { "-" } else { "" };
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting() {
        assert_eq!(fmt_number(1234567.891, None), "1,234,567.891");
        assert_eq!(fmt_number(0.5, None), "0.5");
        assert_eq!(fmt_number(1.0 / 3.0, None), "0.3333");
        assert_eq!(fmt_number(-42.0, None), "-42");
        assert_eq!(fmt_number(12.5, Some(2)), "12.50");
        assert_eq!(fmt_number(0.000123456, None), "0.0001235");
    }
}
