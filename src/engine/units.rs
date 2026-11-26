//! Unit and currency tables.
//!
//! Every quantity is stored in SI base units together with a dimension vector.
//! A unit is a scale factor (and, for temperatures, an offset) relative to the
//! base unit of its dimension.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub const DIM_COUNT: usize = 7;
pub type Dims = [i8; DIM_COUNT];

pub const LENGTH: usize = 0;
pub const MASS: usize = 1;
pub const TIME: usize = 2;
pub const DATA: usize = 3;
pub const TEMP: usize = 4;
pub const MONEY: usize = 5;
pub const ANGLE: usize = 6;

pub const NO_DIMS: Dims = [0; DIM_COUNT];

pub fn dim(index: usize, power: i8) -> Dims {
    let mut d = NO_DIMS;
    d[index] = power;
    d
}

#[derive(Debug, Clone)]
pub struct Unit {
    /// Canonical display symbol.
    pub symbol: String,
    /// Multiply by this to get the SI base value (after adding `offset`).
    pub factor: f64,
    /// Added before scaling (temperatures only).
    pub offset: f64,
    pub dims: Dims,
    /// Currency code when this unit is money.
    pub currency: Option<String>,
    /// Composite units (km/h, m²) expanded into simple ones for arithmetic.
    pub expand: Vec<(UnitId, i8)>,
}

impl Unit {
    pub fn is_currency(&self) -> bool {
        self.currency.is_some()
    }
}

pub type UnitId = usize;

pub struct UnitTable {
    pub units: Vec<Unit>,
    /// Case-sensitive aliases.
    exact: HashMap<String, UnitId>,
    /// Lower-cased aliases, used when no exact match exists.
    folded: HashMap<String, UnitId>,
    currency_by_code: HashMap<String, UnitId>,
}

impl UnitTable {
    pub fn lookup(&self, word: &str) -> Option<UnitId> {
        if let Some(id) = self.exact.get(word) {
            return Some(*id);
        }
        self.folded.get(&word.to_lowercase()).copied()
    }

    pub fn currency(&self, code: &str) -> Option<UnitId> {
        self.currency_by_code.get(&code.to_uppercase()).copied()
    }

    pub fn get(&self, id: UnitId) -> &Unit {
        &self.units[id]
    }

    /// Value of one unit of `code` in USD, honoring live rates if loaded.
    fn currency_factor(&self, id: UnitId) -> f64 {
        let unit = &self.units[id];
        if let Some(code) = &unit.currency
            && let Some(rate) = live_rates().read().ok().and_then(|r| r.get(code).copied())
        {
            return rate;
        }
        unit.factor
    }

    pub fn factor(&self, id: UnitId) -> f64 {
        if self.units[id].is_currency() {
            self.currency_factor(id)
        } else {
            self.units[id].factor
        }
    }
}

/// Live exchange rates (value of one unit of currency, in USD).
pub fn live_rates() -> &'static RwLock<HashMap<String, f64>> {
    static RATES: OnceLock<RwLock<HashMap<String, f64>>> = OnceLock::new();
    RATES.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn table() -> &'static UnitTable {
    static TABLE: OnceLock<UnitTable> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

struct Builder {
    units: Vec<Unit>,
    exact: HashMap<String, UnitId>,
    folded: HashMap<String, UnitId>,
    currency_by_code: HashMap<String, UnitId>,
}

impl Builder {
    /// `exact_aliases` are matched case-sensitively; `aliases` case-insensitively.
    fn add(
        &mut self,
        symbol: &str,
        factor: f64,
        offset: f64,
        dims: Dims,
        exact_aliases: &[&str],
        aliases: &[&str],
    ) -> UnitId {
        let id = self.units.len();
        self.units.push(Unit {
            symbol: symbol.to_string(),
            factor,
            offset,
            dims,
            currency: None,
            expand: Vec::new(),
        });
        for a in exact_aliases {
            self.exact.insert((*a).to_string(), id);
        }
        for a in aliases {
            self.folded.insert(a.to_lowercase(), id);
        }
        id
    }

    fn simple(&mut self, symbol: &str, factor: f64, dims: Dims, aliases: &[&str]) -> UnitId {
        self.add(symbol, factor, 0.0, dims, &[symbol], aliases)
    }

    fn currency(&mut self, code: &str, usd_value: f64, aliases: &[&str]) -> UnitId {
        let id = self.units.len();
        self.units.push(Unit {
            symbol: code.to_string(),
            factor: usd_value,
            offset: 0.0,
            dims: dim(MONEY, 1),
            currency: Some(code.to_string()),
            expand: Vec::new(),
        });
        self.exact.insert(code.to_string(), id);
        self.folded.insert(code.to_lowercase(), id);
        for a in aliases {
            self.folded.insert(a.to_lowercase(), id);
        }
        self.currency_by_code.insert(code.to_string(), id);
        id
    }
}

fn build_table() -> UnitTable {
    let mut b = Builder {
        units: Vec::new(),
        exact: HashMap::new(),
        folded: HashMap::new(),
        currency_by_code: HashMap::new(),
    };
    let l = dim(LENGTH, 1);
    let m = dim(MASS, 1);
    let t = dim(TIME, 1);
    let d = dim(DATA, 1);
    let a = dim(ANGLE, 1);
    let mut area = NO_DIMS;
    area[LENGTH] = 2;
    let mut vol = NO_DIMS;
    vol[LENGTH] = 3;
    let mut speed = NO_DIMS;
    speed[LENGTH] = 1;
    speed[TIME] = -1;

    // Length
    b.simple(
        "nm",
        1e-9,
        l,
        &["nanometer", "nanometers", "nanometre", "nanometres"],
    );
    b.simple(
        "µm",
        1e-6,
        l,
        &["um", "micrometer", "micrometers", "micron", "microns"],
    );
    b.simple(
        "mm",
        1e-3,
        l,
        &["millimeter", "millimeters", "millimetre", "millimetres"],
    );
    b.simple(
        "cm",
        1e-2,
        l,
        &["centimeter", "centimeters", "centimetre", "centimetres"],
    );
    b.simple("m", 1.0, l, &["meter", "meters", "metre", "metres"]);
    b.simple(
        "km",
        1e3,
        l,
        &["kilometer", "kilometers", "kilometre", "kilometres", "kms"],
    );
    b.simple("in", 0.0254, l, &["inch", "inches"]);
    b.simple("ft", 0.3048, l, &["foot", "feet"]);
    b.simple("yd", 0.9144, l, &["yard", "yards", "yds"]);
    b.simple("mi", 1609.344, l, &["mile", "miles"]);
    b.simple("nmi", 1852.0, l, &["nautical mile", "nautical miles"]);
    b.simple(
        "ly",
        9.4607e15,
        l,
        &["lightyear", "lightyears", "light year", "light years"],
    );
    b.simple("au", 1.495978707e11, l, &[]);

    // Mass
    b.simple("mg", 1e-6, m, &["milligram", "milligrams"]);
    b.simple("g", 1e-3, m, &["gram", "grams", "gr"]);
    b.simple(
        "kg",
        1.0,
        m,
        &["kilogram", "kilograms", "kilo", "kilos", "kgs"],
    );
    b.simple("t", 1e3, m, &["tonne", "tonnes", "ton", "tons"]);
    b.simple("oz", 0.028349523125, m, &["ounce", "ounces"]);
    b.simple("lb", 0.45359237, m, &["lbs", "pound", "pounds"]);
    b.add("st", 6.35029318, 0.0, m, &[], &["stone", "stones"]);

    // Time
    b.simple("ns", 1e-9, t, &["nanosecond", "nanoseconds"]);
    b.simple("ms", 1e-3, t, &["millisecond", "milliseconds", "msec"]);
    b.simple("s", 1.0, t, &["sec", "secs", "second", "seconds"]);
    b.simple("min", 60.0, t, &["mins", "minute", "minutes"]);
    b.simple("h", 3600.0, t, &["hr", "hrs", "hour", "hours"]);
    b.simple("day", 86400.0, t, &["days", "d"]);
    b.simple("week", 604800.0, t, &["weeks", "wk", "wks"]);
    b.simple("month", 2629746.0, t, &["months", "mo"]);
    b.simple("year", 31556952.0, t, &["years", "yr", "yrs", "y"]);

    // Data (base: byte)
    b.add("bit", 0.125, 0.0, d, &[], &["bit", "bits"]);
    b.add("B", 1.0, 0.0, d, &["B"], &["byte", "bytes"]);
    b.add("KB", 1e3, 0.0, d, &[], &["kb", "kilobyte", "kilobytes"]);
    b.add("MB", 1e6, 0.0, d, &[], &["mb", "megabyte", "megabytes"]);
    b.add("GB", 1e9, 0.0, d, &[], &["gb", "gigabyte", "gigabytes"]);
    b.add("TB", 1e12, 0.0, d, &[], &["tb", "terabyte", "terabytes"]);
    b.add("PB", 1e15, 0.0, d, &[], &["pb", "petabyte", "petabytes"]);
    b.add(
        "KiB",
        1024.0,
        0.0,
        d,
        &[],
        &["kib", "kibibyte", "kibibytes"],
    );
    b.add(
        "MiB",
        1048576.0,
        0.0,
        d,
        &[],
        &["mib", "mebibyte", "mebibytes"],
    );
    b.add(
        "GiB",
        1073741824.0,
        0.0,
        d,
        &[],
        &["gib", "gibibyte", "gibibytes"],
    );
    b.add(
        "TiB",
        1099511627776.0,
        0.0,
        d,
        &[],
        &["tib", "tebibyte", "tebibytes"],
    );

    // Temperature (base: kelvin)
    let k = dim(TEMP, 1);
    b.add("K", 1.0, 0.0, k, &["K"], &["kelvin", "kelvins"]);
    b.add(
        "°C",
        1.0,
        273.15,
        k,
        &["C", "°C", "ºC"],
        &["celsius", "centigrade", "°c", "degc"],
    );
    b.add(
        "°F",
        5.0 / 9.0,
        459.67,
        k,
        &["F", "°F", "ºF"],
        &["fahrenheit", "°f", "degf"],
    );

    // Angle (base: radian)
    b.simple("rad", 1.0, a, &["radian", "radians"]);
    b.simple(
        "°",
        std::f64::consts::PI / 180.0,
        a,
        &["deg", "degree", "degrees"],
    );

    // Area
    b.simple("ha", 1e4, area, &["hectare", "hectares"]);
    b.simple("acre", 4046.8564224, area, &["acres", "ac"]);
    b.simple("m²", 1.0, area, &["sqm", "sq m"]);
    b.simple("km²", 1e6, area, &["sqkm", "sq km"]);
    b.simple("ft²", 0.09290304, area, &["sqft", "sq ft"]);
    b.simple("mi²", 2589988.110336, area, &["sqmi", "sq mi"]);

    // Volume (base: m³)
    b.simple(
        "ml",
        1e-6,
        vol,
        &[
            "mL",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres",
        ],
    );
    b.simple(
        "cl",
        1e-5,
        vol,
        &["centiliter", "centiliters", "centilitre", "centilitres"],
    );
    b.simple("l", 1e-3, vol, &["L", "liter", "liters", "litre", "litres"]);
    b.simple("gal", 0.003785411784, vol, &["gallon", "gallons"]);
    b.simple("qt", 0.000946352946, vol, &["quart", "quarts"]);
    b.simple("pt", 0.000473176473, vol, &["pint", "pints"]);
    b.simple("cup", 0.0002365882365, vol, &["cups"]);
    b.simple(
        "floz",
        2.95735295625e-5,
        vol,
        &["fl oz", "fluid ounce", "fluid ounces"],
    );
    b.simple(
        "tbsp",
        1.478676478125e-5,
        vol,
        &["tablespoon", "tablespoons"],
    );
    b.simple("tsp", 4.92892159375e-6, vol, &["teaspoon", "teaspoons"]);

    // Speed
    b.simple("km/h", 1000.0 / 3600.0, speed, &["kph", "kmh", "kmph"]);
    b.simple("mph", 1609.344 / 3600.0, speed, &[]);
    b.simple("m/s", 1.0, speed, &["mps"]);
    b.simple("kn", 1852.0 / 3600.0, speed, &["knot", "knots", "kt"]);

    // Currencies: approximate value of one unit in USD (offline fallback).
    b.currency("USD", 1.0, &["dollar", "dollars", "usd", "bucks"]);
    b.currency("EUR", 1.16, &["euro", "euros"]);
    b.currency("GBP", 1.33, &["sterling", "quid"]);
    b.currency("JPY", 0.0066, &["yen"]);
    b.currency("CNY", 0.14, &["yuan", "rmb", "renminbi"]);
    b.currency("INR", 0.0113, &["rupee", "rupees"]);
    b.currency("CAD", 0.72, &[]);
    b.currency("AUD", 0.66, &[]);
    b.currency("NZD", 0.57, &[]);
    b.currency("CHF", 1.25, &["franc", "francs"]);
    b.currency("SEK", 0.105, &[]);
    b.currency("NOK", 0.099, &[]);
    b.currency("DKK", 0.155, &[]);
    b.currency("PLN", 0.27, &["zloty"]);
    b.currency("CZK", 0.047, &[]);
    b.currency("HUF", 0.0029, &[]);
    b.currency("TRY", 0.024, &["lira"]);
    b.currency("RUB", 0.0125, &["ruble", "rubles", "rouble", "roubles"]);
    b.currency("ZAR", 0.057, &["rand"]);
    b.currency("BRL", 0.19, &["reais"]);
    b.currency("MXN", 0.054, &["peso", "pesos"]);
    b.currency("KRW", 0.00069, &[]);
    b.currency("SGD", 0.77, &[]);
    b.currency("HKD", 0.128, &[]);
    b.currency("AED", 0.2723, &["dirham", "dirhams"]);
    b.currency("SAR", 0.2667, &["riyal", "riyals"]);
    b.currency("MAD", 0.108, &[]);
    b.currency("TND", 0.34, &["dinar", "dinars"]);
    b.currency("DZD", 0.0077, &[]);
    b.currency("EGP", 0.021, &[]);
    b.currency("ILS", 0.30, &["shekel", "shekels"]);
    b.currency("THB", 0.031, &["baht"]);
    b.currency("IDR", 0.000061, &["rupiah"]);
    b.currency("PHP", 0.017, &[]);
    b.currency("VND", 0.000038, &["dong"]);

    let mut table = UnitTable {
        units: b.units,
        exact: b.exact,
        folded: b.folded,
        currency_by_code: b.currency_by_code,
    };
    let expansions: &[(&str, &[(&str, i8)])] = &[
        ("km/h", &[("km", 1), ("h", -1)]),
        ("mph", &[("mi", 1), ("h", -1)]),
        ("m/s", &[("m", 1), ("s", -1)]),
        ("kn", &[("nmi", 1), ("h", -1)]),
        ("m²", &[("m", 2)]),
        ("km²", &[("km", 2)]),
        ("ft²", &[("ft", 2)]),
        ("mi²", &[("mi", 2)]),
    ];
    for (composite, parts) in expansions {
        let id = table.lookup(composite).expect("composite unit");
        let expand = parts
            .iter()
            .map(|(sym, p)| (table.lookup(sym).expect("unit part"), *p))
            .collect();
        table.units[id].expand = expand;
    }
    table
}

/// Currency code for a prefix/suffix symbol character.
pub fn currency_for_symbol(c: char) -> Option<&'static str> {
    Some(match c {
        '$' => "USD",
        '€' => "EUR",
        '£' => "GBP",
        '¥' => "JPY",
        '₹' => "INR",
        '₩' => "KRW",
        '₽' => "RUB",
        '₺' => "TRY",
        '₪' => "ILS",
        '฿' => "THB",
        _ => return None,
    })
}

/// Display symbol placed before the amount, if the currency has one.
pub fn currency_prefix(code: &str) -> Option<&'static str> {
    Some(match code {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        "INR" => "₹",
        "KRW" => "₩",
        "RUB" => "₽",
        "TRY" => "₺",
        "ILS" => "₪",
        _ => return None,
    })
}

/// Preferred named unit for a dimension vector (used when a composite unit
/// can't be displayed nicely).
pub fn preferred_unit_for(dims: &Dims) -> Option<UnitId> {
    let t = table();
    let symbol = match *dims {
        [1, 0, 0, 0, 0, 0, 0] => "m",
        [0, 1, 0, 0, 0, 0, 0] => "kg",
        [0, 0, 1, 0, 0, 0, 0] => "s",
        [0, 0, 0, 1, 0, 0, 0] => "B",
        [0, 0, 0, 0, 1, 0, 0] => "K",
        [0, 0, 0, 0, 0, 1, 0] => "USD",
        [0, 0, 0, 0, 0, 0, 1] => "rad",
        [2, 0, 0, 0, 0, 0, 0] => "m²",
        [3, 0, 0, 0, 0, 0, 0] => "l",
        [1, 0, -1, 0, 0, 0, 0] => "m/s",
        _ => return None,
    };
    t.lookup(symbol)
}
