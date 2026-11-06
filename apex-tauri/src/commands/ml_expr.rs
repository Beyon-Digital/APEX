//! The formula language for custom ML features.
//!
//! One formula is evaluated over a series of bars and gives one number per bar. The same code
//! computes training data, dataset previews and live scoring (through [`super::ml_features`]).
//!
//! # Language
//! * numbers: `1`, `0.5`, `.5`, `1e-3`
//! * names: `open high low close volume`
//! * operators: `+ - * /`, unary `-`, comparisons `> < >= <=` (the result is 1 or 0), parentheses.
//!   Comparisons bind loosest, then `+ -`, then `* /`, then unary minus.
//! * functions over a trailing window (each takes a series `x` and an integer literal `n`):
//!   `sma ema std sum max min zscore` (n required), `rsi` (n defaults to 14), `ret` (n defaults to
//!   1, `x / shift(x, n) - 1`), `shift` (n >= 0, the value `n` bars earlier)
//! * `abs(x) log(x) sqrt(x)`, and `atr(n)` (n defaults to 14, Wilder's average true range)
//! * `other("SYMBOL", "field")`: a field of a context symbol, lined up by timestamp. When the
//!   context symbol has no bar at a timestamp, the last bar at or before it is used (never a later
//!   one).
//!
//! # Guarantees
//! * No lookahead: every function reads only the current bar and earlier ones, so the value at bar
//!   `i` is a function of bars `0..=i` by construction.
//! * Every result is finite or NaN. NaN propagates; division by zero, `log` of a non-positive
//!   number, `sqrt` of a negative number and overflow all give NaN. Nothing here panics.
//! * A window function is NaN until its whole window is defined. After a NaN, `ema`, `rsi` and
//!   `atr` start over.
//! * Limits: 300 characters, nesting depth 20, windows 1..=500, total warmup 1800 bars (for
//!   exponential averages, RSI and ATR the warmup includes the bars the starting seed needs to fade).

use apex_core::domain::models::OHLCV;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fmt;

pub const MAX_LEN: usize = 300;
pub const MAX_DEPTH: usize = 20;
pub const MAX_WINDOW: usize = 500;
/// The most history a formula may need before its value is settled. Scoring loads at most 2000
/// bars, so this leaves room for the lag and a margin.
pub const MAX_WARMUP: usize = 1800;
/// How small the starting value's influence on a recursive indicator must be to call it settled.
const SETTLE_EPS: f64 = 0.01;
const DEFAULT_RSI: usize = 14;
const DEFAULT_ATR: usize = 14;

const FUNCTIONS: &str = "sma, ema, std, sum, max, min, zscore, rsi, ret, shift, abs, log, sqrt, atr, other";
const FIELD_NAMES: &str = "open, high, low, close, volume";

/// Bars of the context symbols, keyed by upper-case symbol.
pub type Context = HashMap<String, Vec<OHLCV>>;

/// A parse or evaluation error. `pos` is the 0-based character offset in the formula.
#[derive(Debug, Clone, PartialEq)]
pub struct ExprError {
    pub message: String,
    pub pos: Option<usize>,
}

impl ExprError {
    fn at(pos: usize, message: impl Into<String>) -> Self {
        Self { message: message.into(), pos: Some(pos) }
    }

    fn plain(message: impl Into<String>) -> Self {
        Self { message: message.into(), pos: None }
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pos {
            Some(p) => write!(f, "{} (at character {})", self.message, p + 1),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for ExprError {}

// ── Tokens ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Str(String),
    Plus,
    Minus,
    Star,
    Slash,
    Gt,
    Lt,
    Ge,
    Le,
    LParen,
    RParen,
    Comma,
    End,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    pos: usize,
}

fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Num(v) => format!("number {v}"),
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Str(s) => format!("text \"{s}\""),
        Tok::Plus => "`+`".into(),
        Tok::Minus => "`-`".into(),
        Tok::Star => "`*`".into(),
        Tok::Slash => "`/`".into(),
        Tok::Gt => "`>`".into(),
        Tok::Lt => "`<`".into(),
        Tok::Ge => "`>=`".into(),
        Tok::Le => "`<=`".into(),
        Tok::LParen => "`(`".into(),
        Tok::RParen => "`)`".into(),
        Tok::Comma => "`,`".into(),
        Tok::End => "the end of the formula".into(),
    }
}

fn tokenize(chars: &[char]) -> Result<Vec<Token>, ExprError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let pos = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i < chars.len() && chars[i] == '.' {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // Exponent only when digits follow, so `2e` stays a number followed by a name.
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            let text: String = chars[start..i].iter().collect();
            let v: f64 = text
                .parse()
                .map_err(|_| ExprError::at(pos, format!("`{text}` is not a valid number")))?;
            if !v.is_finite() {
                return Err(ExprError::at(pos, format!("number `{text}` is too large")));
            }
            out.push(Token { tok: Tok::Num(v), pos });
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
            out.push(Token { tok: Tok::Ident(text), pos });
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != quote {
                i += 1;
            }
            if i >= chars.len() {
                return Err(ExprError::at(pos, "text is missing its closing quote"));
            }
            let text: String = chars[start..i].iter().collect();
            i += 1;
            out.push(Token { tok: Tok::Str(text), pos });
            continue;
        }
        let tok = match c {
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' => Tok::Comma,
            '>' | '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    i += 1;
                    if c == '>' { Tok::Ge } else { Tok::Le }
                } else if c == '>' {
                    Tok::Gt
                } else {
                    Tok::Lt
                }
            }
            '=' | '!' => {
                return Err(ExprError::at(pos, format!("`{c}` is not supported; compare with > < >= <=")));
            }
            other => return Err(ExprError::at(pos, format!("unexpected character `{other}`"))),
        };
        i += 1;
        out.push(Token { tok, pos });
    }
    out.push(Token { tok: Tok::End, pos: chars.len() });
    Ok(out)
}

// ── Syntax tree ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Open,
    High,
    Low,
    Close,
    Volume,
}

impl Field {
    fn parse(s: &str) -> Option<Field> {
        Some(match s {
            "open" => Field::Open,
            "high" => Field::High,
            "low" => Field::Low,
            "close" => Field::Close,
            "volume" => Field::Volume,
            _ => return None,
        })
    }

    fn of(self, b: &OHLCV) -> f64 {
        match self {
            Field::Open => b.open,
            Field::High => b.high,
            Field::Low => b.low,
            Field::Close => b.close,
            Field::Volume => b.volume as f64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Gt,
    Lt,
    Ge,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MathFn {
    Abs,
    Log,
    Sqrt,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Roll {
    Sma,
    Ema,
    Std,
    Sum,
    Max,
    Min,
    Zscore,
    Rsi,
    Ret,
    Shift,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Num(f64),
    Field(Field),
    Other { symbol: String, field: Field },
    Neg(Box<Node>),
    Math(MathFn, Box<Node>),
    Bin(BinOp, Box<Node>, Box<Node>),
    Roll(Roll, Box<Node>, usize),
    Atr(usize),
}

/// Extra bars an exponentially smoothed value needs after its first value before the starting
/// seed has faded to [`SETTLE_EPS`]: the seed's weight after `m` steps is `(1 - alpha)^m`.
fn settle_bars(alpha: f64) -> usize {
    if !(alpha > 0.0 && alpha < 1.0) {
        return 0; // alpha = 1 forgets the seed at once
    }
    (SETTLE_EPS.ln() / (1.0 - alpha).ln()).ceil().max(0.0) as usize
}

/// Leading bars for which `node` is not yet reliable: no value at all, or (for exponential
/// averages, RSI and ATR, which carry their seed forward) a value still visibly shaped by where
/// the series happened to start. Training drops these rows and scoring loads enough history to
/// get past them, so a long-window formula gives the same number in both places.
fn warmup_of(node: &Node) -> usize {
    match node {
        Node::Num(_) | Node::Field(_) | Node::Other { .. } => 0,
        Node::Neg(x) | Node::Math(_, x) => warmup_of(x),
        Node::Bin(_, a, b) => warmup_of(a).max(warmup_of(b)),
        Node::Roll(kind, x, n) => {
            let inner = warmup_of(x);
            match kind {
                // A window of n values is first complete at index n - 1.
                Roll::Sma | Roll::Std | Roll::Sum | Roll::Max | Roll::Min | Roll::Zscore => inner + n - 1,
                Roll::Ema => inner + n - 1 + settle_bars(2.0 / (*n as f64 + 1.0)),
                // n changes need n + 1 values; Wilder smoothing then forgets its seed at 1/n a bar.
                Roll::Rsi => inner + n + settle_bars(1.0 / *n as f64),
                // A lookback of n needs the value n bars earlier.
                Roll::Ret | Roll::Shift => inner + n,
            }
        }
        Node::Atr(n) => *n + settle_bars(1.0 / *n as f64),
    }
}

fn collect_symbols(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Other { symbol, .. } => {
            if !out.contains(symbol) {
                out.push(symbol.clone());
            }
        }
        Node::Neg(x) | Node::Math(_, x) | Node::Roll(_, x, _) => collect_symbols(x, out),
        Node::Bin(_, a, b) => {
            collect_symbols(a, out);
            collect_symbols(b, out);
        }
        Node::Num(_) | Node::Field(_) | Node::Atr(_) => {}
    }
}


// ── Parser ───────────────────────────────────────────────────────────

enum ArgVal {
    Expr(Node),
    Text(String),
}

type Arg = (ArgVal, usize);

struct Parser {
    toks: Vec<Token>,
    i: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.toks[self.i.min(self.toks.len() - 1)]
    }

    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn enter(&mut self, pos: usize) -> Result<(), ExprError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ExprError::at(pos, format!("formula is nested too deeply (limit {MAX_DEPTH} levels)")));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_comparison(&mut self) -> Result<Node, ExprError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek().tok {
                Tok::Gt => BinOp::Gt,
                Tok::Lt => BinOp::Lt,
                Tok::Ge => BinOp::Ge,
                Tok::Le => BinOp::Le,
                _ => return Ok(left),
            };
            self.next();
            let right = self.parse_additive()?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn parse_additive(&mut self) -> Result<Node, ExprError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek().tok {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(left),
            };
            self.next();
            let right = self.parse_multiplicative()?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn parse_multiplicative(&mut self) -> Result<Node, ExprError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek().tok {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => return Ok(left),
            };
            self.next();
            let right = self.parse_unary()?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn parse_unary(&mut self) -> Result<Node, ExprError> {
        if self.peek().tok == Tok::Minus {
            let pos = self.next().pos;
            self.enter(pos)?;
            let inner = self.parse_unary()?;
            self.leave();
            return Ok(match inner {
                Node::Num(v) => Node::Num(-v),
                other => Node::Neg(Box::new(other)),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Node, ExprError> {
        let t = self.next();
        match t.tok {
            Tok::Num(v) => Ok(Node::Num(v)),
            Tok::LParen => {
                self.enter(t.pos)?;
                let inner = self.parse_comparison()?;
                self.expect_close(t.pos)?;
                self.leave();
                Ok(inner)
            }
            Tok::Ident(name) => {
                if self.peek().tok == Tok::LParen {
                    self.parse_call(&name, t.pos)
                } else if let Some(f) = Field::parse(&name) {
                    Ok(Node::Field(f))
                } else if FUNCTIONS.split(", ").any(|f| f == name) {
                    Err(ExprError::at(
                        t.pos,
                        format!("`{name}` is a function: write it with parentheses, like {name}(close, 20)"),
                    ))
                } else {
                    Err(ExprError::at(t.pos, format!("unknown name `{name}`; use {FIELD_NAMES}")))
                }
            }
            Tok::Str(s) => Err(ExprError::at(
                t.pos,
                format!("text \"{s}\" is only allowed inside other(\"SYMBOL\", \"field\")"),
            )),
            Tok::End => Err(ExprError::at(t.pos, "the formula ends early; a value is missing")),
            other => Err(ExprError::at(t.pos, format!("expected a value but found {}", describe(&other)))),
        }
    }

    fn expect_close(&mut self, open_pos: usize) -> Result<(), ExprError> {
        let t = self.peek().clone();
        match t.tok {
            Tok::RParen => {
                self.next();
                Ok(())
            }
            Tok::End => Err(ExprError::at(open_pos, "this `(` is never closed")),
            other => Err(ExprError::at(t.pos, format!("expected `)` or an operator but found {}", describe(&other)))),
        }
    }

    /// Arguments of a call; the next token is `(`.
    fn parse_args(&mut self) -> Result<Vec<Arg>, ExprError> {
        let open = self.next();
        self.enter(open.pos)?;
        let mut args: Vec<Arg> = Vec::new();
        if self.peek().tok == Tok::RParen {
            self.next();
            self.leave();
            return Ok(args);
        }
        loop {
            let pos = self.peek().pos;
            if let Tok::Str(s) = self.peek().tok.clone() {
                self.next();
                args.push((ArgVal::Text(s), pos));
            } else {
                args.push((ArgVal::Expr(self.parse_comparison()?), pos));
            }
            let t = self.peek().clone();
            match t.tok {
                Tok::Comma => {
                    self.next();
                }
                Tok::RParen => {
                    self.next();
                    break;
                }
                Tok::End => return Err(ExprError::at(open.pos, "this `(` is never closed")),
                other => {
                    return Err(ExprError::at(t.pos, format!("expected `,` or `)` but found {}", describe(&other))));
                }
            }
        }
        self.leave();
        Ok(args)
    }

    fn parse_call(&mut self, name: &str, name_pos: usize) -> Result<Node, ExprError> {
        let args = self.parse_args()?;
        let count_err = |want: &str| {
            ExprError::at(
                name_pos,
                format!("{name}() takes {want} but {} {} given", args.len(), if args.len() == 1 { "was" } else { "were" }),
            )
        };
        match name {
            "sma" | "ema" | "std" | "sum" | "max" | "min" | "zscore" => {
                if args.len() != 2 {
                    return Err(count_err("2 arguments (series, window)"));
                }
                let mut it = args.into_iter();
                let x = series_arg(name, it.next().unwrap())?;
                let n = window_arg(name, it.next().unwrap(), 1)?;
                let kind = match name {
                    "sma" => Roll::Sma,
                    "ema" => Roll::Ema,
                    "std" => Roll::Std,
                    "sum" => Roll::Sum,
                    "max" => Roll::Max,
                    "min" => Roll::Min,
                    _ => Roll::Zscore,
                };
                Ok(Node::Roll(kind, Box::new(x), n))
            }
            "rsi" | "ret" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(count_err("1 or 2 arguments (series, optional window)"));
                }
                let default = if name == "rsi" { DEFAULT_RSI } else { 1 };
                let mut it = args.into_iter();
                let x = series_arg(name, it.next().unwrap())?;
                let n = match it.next() {
                    Some(a) => window_arg(name, a, 1)?,
                    None => default,
                };
                Ok(Node::Roll(if name == "rsi" { Roll::Rsi } else { Roll::Ret }, Box::new(x), n))
            }
            "shift" => {
                if args.len() != 2 {
                    return Err(count_err("2 arguments (series, bars)"));
                }
                let mut it = args.into_iter();
                let x = series_arg(name, it.next().unwrap())?;
                let n = window_arg(name, it.next().unwrap(), 0)?;
                Ok(Node::Roll(Roll::Shift, Box::new(x), n))
            }
            "abs" | "log" | "sqrt" => {
                if args.len() != 1 {
                    return Err(count_err("1 argument"));
                }
                let x = series_arg(name, args.into_iter().next().unwrap())?;
                let f = match name {
                    "abs" => MathFn::Abs,
                    "log" => MathFn::Log,
                    _ => MathFn::Sqrt,
                };
                Ok(Node::Math(f, Box::new(x)))
            }
            "atr" => {
                if args.len() > 1 {
                    return Err(count_err("0 or 1 arguments (optional window)"));
                }
                let n = match args.into_iter().next() {
                    Some(a) => window_arg(name, a, 1)?,
                    None => DEFAULT_ATR,
                };
                Ok(Node::Atr(n))
            }
            "other" => {
                if args.len() != 2 {
                    return Err(count_err("2 arguments (\"SYMBOL\", \"field\")"));
                }
                let mut it = args.into_iter();
                let (sym, sym_pos) = it.next().unwrap();
                let (fld, fld_pos) = it.next().unwrap();
                let ArgVal::Text(symbol) = sym else {
                    return Err(ExprError::at(sym_pos, "the first argument of other() must be a quoted symbol, like \"SPY\""));
                };
                let ArgVal::Text(field) = fld else {
                    return Err(ExprError::at(
                        fld_pos,
                        format!("the second argument of other() must be a quoted field: one of {FIELD_NAMES}"),
                    ));
                };
                let symbol = symbol.trim().to_uppercase();
                if symbol.is_empty()
                    || symbol.len() > 32
                    || !symbol.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '^' | '=' | ':' | '/'))
                {
                    return Err(ExprError::at(sym_pos, format!("`{symbol}` is not a valid symbol")));
                }
                let field = Field::parse(&field.trim().to_ascii_lowercase()).ok_or_else(|| {
                    ExprError::at(fld_pos, format!("unknown field \"{field}\" in other(); use {FIELD_NAMES}"))
                })?;
                Ok(Node::Other { symbol, field })
            }
            _ => Err(ExprError::at(name_pos, format!("unknown function `{name}`; available: {FUNCTIONS}"))),
        }
    }
}

fn series_arg(func: &str, (val, pos): Arg) -> Result<Node, ExprError> {
    match val {
        ArgVal::Expr(n) => Ok(n),
        ArgVal::Text(_) => Err(ExprError::at(pos, format!("{func}() needs a number or series here, not text"))),
    }
}

/// A window must be a plain whole-number literal so the warmup is known before any data is read.
fn window_arg(func: &str, (val, pos): Arg, min: usize) -> Result<usize, ExprError> {
    let v = match val {
        ArgVal::Expr(Node::Num(v)) => v,
        _ => {
            return Err(ExprError::at(
                pos,
                format!("the window of {func}() must be a whole-number literal such as 20, not a formula"),
            ))
        }
    };
    if v.fract() != 0.0 {
        return Err(ExprError::at(pos, format!("the window of {func}() must be a whole number, got {v}")));
    }
    if v < min as f64 || v > MAX_WINDOW as f64 {
        return Err(ExprError::at(
            pos,
            format!("the window of {func}() must be between {min} and {MAX_WINDOW}, got {v}"),
        ));
    }
    Ok(v as usize)
}

// ── Compile ──────────────────────────────────────────────────────────

/// A parsed formula, ready to evaluate over any number of series.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// Leading bars for which the value is NaN (given a fully defined input).
    pub warmup: usize,
    /// Upper-case context symbols the formula reads with `other(...)`, in order of first use.
    pub referenced_symbols: Vec<String>,
    root: Node,
}

pub fn compile(expr: &str) -> Result<Compiled, ExprError> {
    let chars: Vec<char> = expr.chars().collect();
    if chars.iter().all(|c| c.is_whitespace()) {
        return Err(ExprError::at(0, "enter a formula, for example sma(close, 20) / close - 1"));
    }
    if chars.len() > MAX_LEN {
        return Err(ExprError::at(MAX_LEN, format!("formula is too long: {} characters, the limit is {MAX_LEN}", chars.len())));
    }
    let toks = tokenize(&chars)?;
    let mut p = Parser { toks, i: 0, depth: 0 };
    let root = p.parse_comparison()?;
    let t = p.peek().clone();
    if t.tok != Tok::End {
        let hint = match t.tok {
            Tok::RParen => "there is a `)` without a matching `(`".to_string(),
            ref other => format!("unexpected {}; an operator (+ - * / > <) is needed between values", describe(other)),
        };
        return Err(ExprError::at(t.pos, hint));
    }
    let warmup = warmup_of(&root);
    if warmup > MAX_WARMUP {
        return Err(ExprError::plain(format!(
            "formula needs {warmup} bars of history before its value is settled; the limit is {MAX_WARMUP}. Use shorter windows"
        )));
    }
    let mut referenced_symbols = Vec::new();
    collect_symbols(&root, &mut referenced_symbols);
    Ok(Compiled { warmup, referenced_symbols, root })
}

// ── Evaluation ───────────────────────────────────────────────────────

/// Replace infinities with NaN so non-finite values never escape.
#[inline]
fn fin(v: f64) -> f64 {
    if v.is_finite() { v } else { f64::NAN }
}

struct Frame<'a> {
    n: usize,
    bars: &'a [OHLCV],
    context: &'a Context,
    own: HashMap<Field, Vec<f64>>,
    others: HashMap<(String, Field), Vec<f64>>,
}

impl<'a> Frame<'a> {
    fn field(&mut self, f: Field) -> Vec<f64> {
        let bars = self.bars;
        self.own
            .entry(f)
            .or_insert_with(|| bars.iter().map(|b| fin(f.of(b))).collect())
            .clone()
    }

    /// Values of `symbol.field` at each of this series' timestamps, using the last bar at or
    /// before it (never a later one). NaN before the symbol's first bar.
    fn other(&mut self, symbol: &str, f: Field) -> Result<Vec<f64>, ExprError> {
        let key = (symbol.to_string(), f);
        if let Some(v) = self.others.get(&key) {
            return Ok(v.clone());
        }
        let ctx = self.context.get(symbol).ok_or_else(|| {
            ExprError::plain(format!(
                "no bars for {symbol}: other(\"{symbol}\", ...) needs {symbol} as a context symbol with stored bars \
                 (fetch its history first)"
            ))
        })?;
        let mut series: Vec<(DateTime<Utc>, f64)> = ctx.iter().map(|b| (b.time, fin(f.of(b)))).collect();
        series.sort_by_key(|(t, _)| *t);
        let out: Vec<f64> = self
            .bars
            .iter()
            .map(|b| {
                let k = series.partition_point(|(t, _)| *t <= b.time);
                if k == 0 { f64::NAN } else { series[k - 1].1 }
            })
            .collect();
        self.others.insert(key, out.clone());
        Ok(out)
    }
}

/// Compute `compiled` for every bar. The result has one value per bar (NaN where undefined).
/// Fails only when an `other(...)` symbol has no bars in `context`.
pub fn evaluate(compiled: &Compiled, bars: &[OHLCV], context: &Context) -> Result<Vec<f64>, ExprError> {
    let mut frame = Frame { n: bars.len(), bars, context, own: HashMap::new(), others: HashMap::new() };
    eval(&compiled.root, &mut frame)
}

fn eval(node: &Node, fr: &mut Frame) -> Result<Vec<f64>, ExprError> {
    let n = fr.n;
    Ok(match node {
        Node::Num(v) => vec![*v; n],
        Node::Field(f) => fr.field(*f),
        Node::Other { symbol, field } => fr.other(symbol, *field)?,
        Node::Neg(x) => eval(x, fr)?.into_iter().map(|v| -v).collect(),
        Node::Math(f, x) => {
            let xs = eval(x, fr)?;
            xs.into_iter()
                .map(|v| match f {
                    MathFn::Abs => v.abs(),
                    MathFn::Log => if v > 0.0 { fin(v.ln()) } else { f64::NAN },
                    MathFn::Sqrt => if v >= 0.0 { fin(v.sqrt()) } else { f64::NAN },
                })
                .collect()
        }
        Node::Bin(op, a, b) => {
            let (a, b) = (eval(a, fr)?, eval(b, fr)?);
            a.iter().zip(&b).map(|(&x, &y)| binary(*op, x, y)).collect()
        }
        Node::Roll(kind, x, w) => {
            let xs = eval(x, fr)?;
            roll(*kind, &xs, *w)
        }
        Node::Atr(w) => atr(fr.bars, *w),
    })
}

fn binary(op: BinOp, x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    match op {
        BinOp::Add => fin(x + y),
        BinOp::Sub => fin(x - y),
        BinOp::Mul => fin(x * y),
        BinOp::Div => if y == 0.0 { f64::NAN } else { fin(x / y) },
        BinOp::Gt => flag(x > y),
        BinOp::Lt => flag(x < y),
        BinOp::Ge => flag(x >= y),
        BinOp::Le => flag(x <= y),
    }
}

fn roll(kind: Roll, x: &[f64], w: usize) -> Vec<f64> {
    match kind {
        Roll::Sma => windowed(x, w, |s| s.iter().sum::<f64>() / s.len() as f64),
        Roll::Sum => windowed(x, w, |s| s.iter().sum::<f64>()),
        Roll::Max => windowed(x, w, |s| s.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
        Roll::Min => windowed(x, w, |s| s.iter().cloned().fold(f64::INFINITY, f64::min)),
        Roll::Std => windowed(x, w, |s| mean_sd(s).1),
        Roll::Zscore => windowed(x, w, |s| {
            let (mean, sd) = mean_sd(s);
            if sd <= 1e-12 * mean.abs() || sd == 0.0 {
                f64::NAN
            } else {
                (s[s.len() - 1] - mean) / sd
            }
        }),
        Roll::Ema => ema(x, w),
        Roll::Rsi => rsi(x, w),
        Roll::Ret => (0..x.len())
            .map(|i| if i >= w { binary(BinOp::Div, x[i], x[i - w]) - 1.0 } else { f64::NAN })
            .map(fin)
            .collect(),
        Roll::Shift => (0..x.len()).map(|i| if i >= w { x[i - w] } else { f64::NAN }).collect(),
    }
}

/// Population mean and standard deviation of a window.
fn mean_sd(s: &[f64]) -> (f64, f64) {
    let n = s.len() as f64;
    let mean = s.iter().sum::<f64>() / n;
    let var = s.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    (mean, var.sqrt())
}

/// Apply `f` to each full trailing window; NaN until the window is full and while any value in it
/// is NaN.
fn windowed(x: &[f64], w: usize, f: impl Fn(&[f64]) -> f64) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    let mut bad = 0usize; // non-finite values inside the current window
    for i in 0..x.len() {
        if !x[i].is_finite() {
            bad += 1;
        }
        if i >= w && !x[i - w].is_finite() {
            bad -= 1;
        }
        if i + 1 >= w && bad == 0 {
            out[i] = fin(f(&x[i + 1 - w..=i]));
        }
    }
    out
}

/// Exponential moving average seeded with the simple average of the first `w` values; restarts
/// after a NaN.
fn ema(x: &[f64], w: usize) -> Vec<f64> {
    let k = 2.0 / (w as f64 + 1.0);
    let mut out = vec![f64::NAN; x.len()];
    let mut run = 0usize;
    let mut prev = f64::NAN;
    for i in 0..x.len() {
        if !x[i].is_finite() {
            run = 0;
            continue;
        }
        run += 1;
        if run < w {
            continue;
        }
        prev = if run == w {
            x[i + 1 - w..=i].iter().sum::<f64>() / w as f64
        } else {
            (x[i] - prev) * k + prev
        };
        if !prev.is_finite() {
            run = 0;
            continue;
        }
        out[i] = prev;
    }
    out
}

/// Wilder's relative strength index of a series, 0 to 100.
fn rsi(x: &[f64], w: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    let (mut gain, mut loss) = (0.0f64, 0.0f64);
    let mut run = 0usize; // consecutive valid changes
    let score = |g: f64, l: f64| {
        if l == 0.0 {
            if g == 0.0 { 50.0 } else { 100.0 }
        } else {
            100.0 - 100.0 / (1.0 + g / l)
        }
    };
    for i in 1..x.len() {
        if !x[i].is_finite() || !x[i - 1].is_finite() {
            run = 0;
            continue;
        }
        run += 1;
        if run < w {
            continue;
        }
        if run == w {
            let (mut g, mut l) = (0.0, 0.0);
            for j in i + 1 - w..=i {
                let d = x[j] - x[j - 1];
                if d > 0.0 { g += d } else { l -= d }
            }
            gain = g / w as f64;
            loss = l / w as f64;
        } else {
            let d = x[i] - x[i - 1];
            let (g, l) = if d > 0.0 { (d, 0.0) } else { (0.0, -d) };
            gain = (gain * (w as f64 - 1.0) + g) / w as f64;
            loss = (loss * (w as f64 - 1.0) + l) / w as f64;
        }
        if gain.is_finite() && loss.is_finite() {
            out[i] = score(gain, loss);
        } else {
            run = 0;
        }
    }
    out
}

/// Wilder's average true range of the bars.
fn atr(bars: &[OHLCV], w: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; bars.len()];
    let mut tr = vec![f64::NAN; bars.len()];
    for i in 1..bars.len() {
        let (h, l, pc) = (bars[i].high, bars[i].low, bars[i - 1].close);
        if !(h.is_finite() && l.is_finite() && pc.is_finite()) {
            continue; // f64::max would silently skip a NaN
        }
        tr[i] = fin((h - l).max((h - pc).abs()).max((l - pc).abs()));
    }
    let mut run = 0usize;
    let mut prev = f64::NAN;
    for i in 1..bars.len() {
        if !tr[i].is_finite() {
            run = 0;
            continue;
        }
        run += 1;
        if run < w {
            continue;
        }
        prev = if run == w {
            tr[i + 1 - w..=i].iter().sum::<f64>() / w as f64
        } else {
            (prev * (w as f64 - 1.0) + tr[i]) / w as f64
        };
        if !prev.is_finite() {
            run = 0;
            continue;
        }
        out[i] = prev;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::ml_features::tests::synthetic_bars;
    use super::*;
    use apex_core::application::indicators as ind;
    use apex_core::domain::models::Symbol;
    use chrono::{Duration, TimeZone};

    fn bars_from_closes(closes: &[f64]) -> Vec<OHLCV> {
        let mut bars = synthetic_bars(closes.len());
        for (b, c) in bars.iter_mut().zip(closes) {
            b.close = *c;
            b.open = *c;
            b.high = *c + 1.0;
            b.low = *c - 1.0;
        }
        bars
    }

    fn no_ctx() -> Context {
        Context::new()
    }

    fn run(expr: &str, bars: &[OHLCV]) -> Vec<f64> {
        let c = compile(expr).unwrap_or_else(|e| panic!("`{expr}` should compile: {e}"));
        evaluate(&c, bars, &no_ctx()).unwrap_or_else(|e| panic!("`{expr}` should evaluate: {e}"))
    }

    fn last(expr: &str, bars: &[OHLCV]) -> f64 {
        *run(expr, bars).last().unwrap()
    }

    fn approx(a: f64, b: f64) -> bool {
        (a.is_nan() && b.is_nan()) || (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    fn err(expr: &str) -> ExprError {
        compile(expr).expect_err(&format!("`{expr}` should not compile"))
    }

    #[test]
    fn numbers_precedence_and_unary_minus() {
        let b = synthetic_bars(5);
        for (expr, want) in [
            ("1 + 2 * 3", 7.0),
            ("(1 + 2) * 3", 9.0),
            ("10 - 4 - 3", 3.0),
            ("2 * 3 / 4", 1.5),
            ("8 / 4 / 2", 1.0),
            ("-2 * 3", -6.0),
            ("- - 4", 4.0),
            ("2 - -3", 5.0),
            ("-(1 + 2)", -3.0),
            ("1e2", 100.0),
            ("1.5e-1", 0.15),
            (".5", 0.5),
            ("2 * -3 + 10", 4.0),
        ] {
            assert!(approx(last(expr, &b), want), "{expr}");
        }
        let closes = [3.0, 5.0];
        let b = bars_from_closes(&closes);
        assert_eq!(last("-close", &b), -5.0);
        assert_eq!(last("--close", &b), 5.0);
        assert_eq!(last("close - 1", &b), 4.0);
    }

    #[test]
    fn comparisons_give_one_or_zero_and_bind_loosest() {
        let b = synthetic_bars(3);
        for (expr, want) in [
            ("3 > 2", 1.0),
            ("2 > 3", 0.0),
            ("3 >= 3", 1.0),
            ("3 > 3", 0.0),
            ("2 < 3", 1.0),
            ("3 <= 3", 1.0),
            ("4 <= 3", 0.0),
            ("1 + 2 > 2", 1.0),
            ("1 > 2 + 5", 0.0),
            ("-2 > -3", 1.0),
            ("2 * 3 >= 6", 1.0),
            ("(1 < 2) + (2 < 3)", 2.0),
            ("1 < 2 < 3", 1.0),
        ] {
            assert_eq!(last(expr, &b), want, "{expr}");
        }
        let b = bars_from_closes(&[1.0, 2.0, 3.0, 2.0, 1.0]);
        assert_eq!(run("close > shift(close, 1)", &b)[1..], [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn names_resolve_to_bar_fields() {
        let b = synthetic_bars(10);
        let i = 9;
        assert_eq!(last("open", &b), b[i].open);
        assert_eq!(last("high", &b), b[i].high);
        assert_eq!(last("low", &b), b[i].low);
        assert_eq!(last("close", &b), b[i].close);
        assert_eq!(last("volume", &b), b[i].volume as f64);
        assert_eq!(last("CLOSE", &b), b[i].close, "names are case-insensitive");
    }

    #[test]
    fn window_functions_match_hand_computation() {
        let closes: Vec<f64> = vec![4.0, 8.0, 6.0, 2.0, 10.0, 12.0];
        let b = bars_from_closes(&closes);
        // Window of 3 ending at the last bar: 2, 10, 12.
        assert!(approx(last("sma(close, 3)", &b), 8.0));
        assert!(approx(last("sum(close, 3)", &b), 24.0));
        assert!(approx(last("max(close, 3)", &b), 12.0));
        assert!(approx(last("min(close, 3)", &b), 2.0));
        let sd = (((2.0f64 - 8.0).powi(2) + (10.0f64 - 8.0).powi(2) + (12.0f64 - 8.0).powi(2)) / 3.0).sqrt();
        assert!(approx(last("std(close, 3)", &b), sd), "population standard deviation");
        assert!(approx(last("zscore(close, 3)", &b), (12.0 - 8.0) / sd));
        assert!(approx(last("shift(close, 2)", &b), 2.0));
        assert!(approx(last("shift(close, 0)", &b), 12.0));
        assert!(approx(last("ret(close, 2)", &b), 12.0 / 2.0 - 1.0));
        assert!(approx(last("ret(close)", &b), 12.0 / 10.0 - 1.0));
        assert!(approx(last("ret(close, 5)", &b), 12.0 / 4.0 - 1.0));
        // Leading NaNs.
        let s = run("sma(close, 3)", &b);
        assert!(s[0].is_nan() && s[1].is_nan() && s[2].is_finite());
        assert!(run("shift(close, 2)", &b)[1].is_nan());
    }

    #[test]
    fn ema_rsi_and_atr_agree_with_the_indicator_library() {
        let b = synthetic_bars(120);
        let close: Vec<f64> = b.iter().map(|x| x.close).collect();
        let high: Vec<f64> = b.iter().map(|x| x.high).collect();
        let low: Vec<f64> = b.iter().map(|x| x.low).collect();

        let got = run("ema(close, 12)", &b);
        let want = ind::ema(&close, 12).unwrap();
        for (k, w) in want.iter().enumerate() {
            assert!(approx(got[11 + k], *w), "ema at {}", 11 + k);
        }
        assert!(got[10].is_nan());

        let got = run("rsi(close, 14)", &b);
        let want = ind::rsi(&close, 14).unwrap();
        for (k, w) in want.iter().enumerate() {
            assert!(approx(got[14 + k], *w), "rsi at {}", 14 + k);
        }
        assert!(got[13].is_nan());
        assert!(got.iter().zip(run("rsi(close)", &b)).all(|(a, b)| approx(*a, b)), "default window is 14");

        let got = run("atr(14)", &b);
        let want = ind::atr(&high, &low, &close, 14).unwrap();
        for (k, w) in want.iter().enumerate() {
            assert!(approx(got[14 + k], *w), "atr at {}", 14 + k);
        }
        assert!(got[13].is_nan());
        assert!(got.iter().zip(run("atr()", &b)).all(|(a, b)| approx(*a, b)), "default window is 14");
    }

    #[test]
    fn rsi_saturates_cleanly() {
        let up: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        assert_eq!(last("rsi(close, 14)", &bars_from_closes(&up)), 100.0);
        let down: Vec<f64> = (0..30).map(|i| 100.0 - i as f64).collect();
        assert_eq!(last("rsi(close, 14)", &bars_from_closes(&down)), 0.0);
        assert_eq!(last("rsi(close, 14)", &bars_from_closes(&[5.0; 30])), 50.0);
    }

    #[test]
    fn math_functions_and_their_domains() {
        let b = bars_from_closes(&[4.0, -9.0, 0.0, 1.0]);
        assert_eq!(run("abs(close)", &b), vec![4.0, 9.0, 0.0, 1.0]);
        let l = run("log(close)", &b);
        assert!(approx(l[0], 4.0f64.ln()) && l[1].is_nan() && l[2].is_nan() && l[3] == 0.0);
        let s = run("sqrt(close)", &b);
        assert_eq!(s[0], 2.0);
        assert!(s[1].is_nan());
        assert_eq!(s[2], 0.0);
    }

    #[test]
    fn division_by_zero_and_overflow_are_nan_not_infinite() {
        let b = bars_from_closes(&[0.0, 2.0, 1e300]);
        let d = run("1 / close", &b);
        assert!(d[0].is_nan() && d[1] == 0.5);
        assert!(run("close * close", &b)[2].is_nan(), "overflow gives NaN");
        assert!(run("(close - close) / (close - close)", &b).iter().all(|v| v.is_nan()));
        assert!(run("ret(close, 1)", &b)[1].is_nan(), "ret from 0 is undefined");
        // NaN propagates through everything and comparisons.
        assert!(run("(1 / 0) > 1", &b).iter().all(|v| v.is_nan()));
        assert!(run("sma(1 / close, 2)", &b)[1].is_nan(), "a NaN inside a window poisons it");
        assert!(run("sma(1 / close, 2)", &b)[2].is_finite());
    }

    #[test]
    fn recursive_indicators_report_the_bars_their_seed_needs_to_fade() {
        // First value at index n-1 (ema) / n (rsi, atr), then enough bars for the seed to fade to 1%.
        for (expr, first, warmup) in [
            ("ema(close, 12)", 11, 39),
            ("ema(close, 200)", 199, 660),
            ("rsi(close, 14)", 14, 77),
            ("rsi(close)", 14, 77),
            ("atr(10)", 10, 54),
            ("atr()", 14, 77),
            ("ema(close, 1)", 0, 0),
        ] {
            let b = synthetic_bars(900);
            let c = compile(expr).unwrap();
            assert_eq!(c.warmup, warmup, "settled warmup of `{expr}`");
            let v = evaluate(&c, &b, &no_ctx()).unwrap();
            let leading = v.iter().take_while(|x| x.is_nan()).count();
            assert_eq!(leading, first, "first value of `{expr}`");
            assert!(c.warmup >= leading);
        }
    }

    #[test]
    fn a_settled_value_does_not_depend_on_where_the_history_starts() {
        // The point of the settling period: a formula scored on only `warmup + 1` bars must agree
        // with the same formula computed over a much longer history, to well under a hundredth of a
        // percent of the value. Without it a window of 300 bars was off by more than 1%.
        let long = synthetic_bars(2400);
        for expr in [
            "ema(close, 200) / close - 1",
            "ema(close, 50) / close - 1",
            "rsi(close, 14)",
            "rsi(close, 60)",
            "atr(14) / close",
            "atr(40) / close",
            "ema(sma(close, 20), 30) / close - 1",
        ] {
            let c = compile(expr).unwrap();
            let full = evaluate(&c, &long, &no_ctx()).unwrap();
            let need = c.warmup + 1;
            let short = &long[long.len() - need..];
            let tail = evaluate(&c, short, &no_ctx()).unwrap();
            let (a, b) = (*full.last().unwrap(), *tail.last().unwrap());
            assert!(a.is_finite() && b.is_finite(), "`{expr}`: {a} {b}");
            let rel = (a - b).abs() / a.abs().max(1e-9);
            assert!(rel < 5e-3, "`{expr}`: full {a} vs {need}-bar window {b} differ by {:.3}%", rel * 100.0);
        }
    }

    #[test]
    fn warmup_is_reported_and_matches_leading_nans() {
        let b = synthetic_bars(300);
        for (expr, warmup) in [
            ("close", 0),
            ("1 + 2", 0),
            ("sma(close, 20)", 19),
            ("std(close, 5)", 4),
            ("ret(close, 5)", 5),
            ("shift(close, 3)", 3),
            ("sma(close, 20) / close - 1", 19),
            ("sma(ret(close), 10)", 10),
            ("sma(sma(close, 5), 5)", 8),
            ("zscore(sma(close, 10), 20) > 0", 28),
            ("close / shift(close, 7) - sma(volume, 30)", 29),
        ] {
            let c = compile(expr).unwrap();
            assert_eq!(c.warmup, warmup, "warmup of `{expr}`");
            let v = evaluate(&c, &b, &no_ctx()).unwrap();
            let leading = v.iter().take_while(|x| x.is_nan()).count();
            assert_eq!(leading, warmup, "leading NaNs of `{expr}`");
            assert!(v[warmup..].iter().all(|x| x.is_finite()), "`{expr}` has a NaN after its warmup");
        }
    }

    #[test]
    fn parse_errors_carry_character_positions() {
        for (expr, pos, fragment) in [
            ("close +", 7, "ends early"),
            ("1 + clos", 4, "unknown name `clos`"),
            ("foo(close)", 0, "unknown function `foo`"),
            ("sma(close)", 0, "takes 2 arguments"),
            ("sma(close, 20, 3)", 0, "takes 2 arguments"),
            ("sma(close, 0)", 11, "between 1 and 500"),
            ("sma(close, 501)", 11, "between 1 and 500"),
            ("sma(close, -3)", 11, "between 1 and 500"),
            ("sma(close, 2.5)", 11, "whole number"),
            ("sma(close, close)", 11, "literal"),
            ("shift(close, -1)", 13, "between 0 and 500"),
            ("(1 + 2", 0, "never closed"),
            ("sma(close, 3", 3, "never closed"),
            ("1 + 2)", 5, "without a matching"),
            ("close close", 6, "operator"),
            ("close = 1", 6, "not supported"),
            ("close $ 2", 6, "unexpected character `$`"),
            ("sum", 0, "is a function"),
            ("\"SPY\"", 0, "only allowed inside other"),
            ("other(\"SPY\")", 0, "takes 2 arguments"),
            ("other(\"SPY\", \"vwap\")", 13, "unknown field"),
            ("other(1, \"close\")", 6, "quoted symbol"),
            ("other(\"SPY\", close)", 13, "quoted field"),
            ("other(\"SPY\", \"close", 13, "closing quote"),
            ("close,", 5, "operator"),
            ("sma(close,, 3)", 10, "expected a value"),
            ("   ", 0, "enter a formula"),
            ("", 0, "enter a formula"),
            ("1e999", 0, "too large"),
        ] {
            let e = err(expr);
            assert_eq!(e.pos, Some(pos), "position for `{expr}`: {e}");
            assert!(e.message.contains(fragment), "message for `{expr}`: {e}");
        }
    }

    #[test]
    fn positions_count_characters_not_bytes() {
        let e = err("é + close");
        assert_eq!(e.pos, Some(0));
        let e = err("close + é");
        assert_eq!(e.pos, Some(8));
    }

    #[test]
    fn display_shows_a_one_based_position() {
        assert!(err("1 + clos").to_string().contains("character 5"));
    }

    #[test]
    fn limits_are_enforced_with_clear_errors() {
        // Length.
        let long = format!("close{}", " + close".repeat(40));
        assert!(long.chars().count() > MAX_LEN);
        assert!(err(&long).message.contains("too long"));
        let at_limit = format!("{}1", "1+".repeat(149)); // 299 chars
        assert!(compile(&at_limit).is_ok());

        // Depth: 20 nested parentheses are fine, 21 are not.
        let nest = |k: usize| format!("{}1{}", "(".repeat(k), ")".repeat(k));
        assert!(compile(&nest(MAX_DEPTH)).is_ok());
        let e = err(&nest(MAX_DEPTH + 1));
        assert!(e.message.contains("nested too deeply") && e.pos == Some(MAX_DEPTH), "{e}");
        // Calls and unary minus count as nesting too.
        let calls = |k: usize| format!("{}close{}", "abs(".repeat(k), ")".repeat(k));
        assert!(compile(&calls(MAX_DEPTH)).is_ok());
        assert!(err(&calls(MAX_DEPTH + 1)).message.contains("nested too deeply"));
        assert!(err(&format!("{}close", "-".repeat(MAX_DEPTH + 1))).message.contains("nested too deeply"));

        // Windows.
        assert!(compile("sma(close, 500)").is_ok());
        assert!(compile("sma(close, 501)").is_err());

        // Total warmup: chained windows add up.
        assert_eq!(compile("sma(sma(close, 500), 500)").unwrap().warmup, 998);
        assert_eq!(compile("sma(sma(sma(close, 500), 500), 500)").unwrap().warmup, 1497);
        assert!(compile("sma(sma(sma(sma(close, 500), 500), 500), 500)").is_err());
        let e = err("sma(sma(sma(sma(close, 500), 500), 500), 500)");
        assert!(e.message.contains("1996") && e.message.contains("1800") && e.pos.is_none(), "{e}");
        assert!(compile("sma(close, 500) + sma(close, 400)").is_ok(), "parallel windows do not add");
    }

    #[test]
    fn referenced_symbols_are_collected_upper_cased_and_unique() {
        let c = compile("other(\"spy\", \"close\") / other(\"QQQ\", \"open\") - other(\"SPY\", 'volume')").unwrap();
        assert_eq!(c.referenced_symbols, vec!["SPY", "QQQ"]);
        assert!(compile("close").unwrap().referenced_symbols.is_empty());
        assert!(compile("other(\"BAD SYM\", \"close\")").is_err());
    }

    // ── Context symbols ──────────────────────────────────────────────

    fn bar_at(symbol: &str, day: i64, close: f64) -> OHLCV {
        OHLCV {
            time: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap() + Duration::days(day),
            symbol: Symbol(symbol.into()),
            timeframe: apex_core::domain::models::Timeframe::D1,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1,
        }
    }

    #[test]
    fn context_values_are_aligned_by_time_and_forward_filled() {
        // Main symbol trades days 0..=6; the context symbol is missing days 2, 3 and 5, and starts
        // on day 1.
        let main: Vec<OHLCV> = (0..7).map(|d| bar_at("AAA", d, 100.0 + d as f64)).collect();
        let ctx_bars = vec![bar_at("SPY", 1, 10.0), bar_at("SPY", 4, 40.0), bar_at("SPY", 6, 60.0)];
        let mut ctx = Context::new();
        ctx.insert("SPY".into(), ctx_bars);
        let c = compile("other(\"SPY\", \"close\")").unwrap();
        let v = evaluate(&c, &main, &ctx).unwrap();
        assert!(v[0].is_nan(), "nothing known before the first context bar");
        assert_eq!(v[1..], [10.0, 10.0, 10.0, 40.0, 40.0, 60.0]);
    }

    #[test]
    fn context_forward_fill_never_uses_a_later_bar() {
        let main: Vec<OHLCV> = (0..5).map(|d| bar_at("AAA", d, 100.0)).collect();
        let mut ctx = Context::new();
        ctx.insert("SPY".into(), vec![bar_at("SPY", 0, 1.0), bar_at("SPY", 3, 999.0)]);
        let c = compile("other(\"SPY\", \"close\")").unwrap();
        let before = evaluate(&c, &main, &ctx).unwrap();
        assert_eq!(before[..3], [1.0, 1.0, 1.0], "the day-3 value is invisible on days 0 to 2");
        // Change the future context bar: earlier values must not move.
        ctx.insert("SPY".into(), vec![bar_at("SPY", 0, 1.0), bar_at("SPY", 3, -5.0)]);
        let after = evaluate(&c, &main, &ctx).unwrap();
        assert_eq!(before[..3], after[..3]);
        assert_eq!(after[3], -5.0);
    }

    #[test]
    fn context_unsorted_input_and_composition_with_other_functions() {
        let main: Vec<OHLCV> = (0..30).map(|d| bar_at("AAA", d, 50.0 + d as f64)).collect();
        let mut shuffled: Vec<OHLCV> = (0..30).map(|d| bar_at("SPY", d, 200.0 + 2.0 * d as f64)).collect();
        shuffled.reverse();
        let mut ctx = Context::new();
        ctx.insert("SPY".into(), shuffled);
        let c = compile("close / other(\"SPY\", \"close\") - sma(other('spy', 'close'), 5) * 0").unwrap();
        assert_eq!(c.referenced_symbols, vec!["SPY"]);
        let v = evaluate(&c, &main, &ctx).unwrap();
        assert!(approx(v[10], 60.0 / 220.0));
        let rel = compile("ret(other(\"SPY\", \"close\"), 3)").unwrap();
        let r = evaluate(&rel, &main, &ctx).unwrap();
        assert!(approx(r[10], 220.0 / 214.0 - 1.0));
        assert_eq!(rel.warmup, 3);
    }

    #[test]
    fn unknown_context_symbol_names_the_symbol() {
        let main = synthetic_bars(10);
        let c = compile("other(\"QQQ\", \"close\")").unwrap();
        let e = evaluate(&c, &main, &no_ctx()).unwrap_err();
        assert!(e.message.contains("QQQ") && e.message.contains("context symbol"), "{e}");
        // A symbol present but with no bars is also an unknown to the user: it still evaluates
        // (all NaN) so the caller decides; the empty case is handled by the dataset layer.
        let mut ctx = Context::new();
        ctx.insert("QQQ".into(), vec![]);
        assert!(evaluate(&c, &main, &ctx).unwrap().iter().all(|v| v.is_nan()));
    }

    // ── No lookahead ─────────────────────────────────────────────────

    /// Deterministic xorshift, so failures reproduce without an external crate.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// A random valid formula built from the grammar.
    fn gen_formula(rng: &mut Rng, depth: usize) -> String {
        let leaf = |rng: &mut Rng| match rng.below(9) {
            0 => "open".to_string(),
            1 => "high".to_string(),
            2 => "low".to_string(),
            3 => "volume".to_string(),
            4 => format!("{}", rng.below(5)),
            5 => "other(\"SPY\", \"close\")".to_string(),
            6 => "atr(7)".to_string(),
            _ => "close".to_string(),
        };
        if depth == 0 {
            return leaf(rng);
        }
        let w = 1 + rng.below(12);
        match rng.below(14) {
            0 => format!("({} + {})", gen_formula(rng, depth - 1), gen_formula(rng, depth - 1)),
            1 => format!("({} - {})", gen_formula(rng, depth - 1), gen_formula(rng, depth - 1)),
            2 => format!("({} * {})", gen_formula(rng, depth - 1), gen_formula(rng, depth - 1)),
            3 => format!("({} / {})", gen_formula(rng, depth - 1), gen_formula(rng, depth - 1)),
            4 => format!("({} > {})", gen_formula(rng, depth - 1), gen_formula(rng, depth - 1)),
            5 => format!("-{}", gen_formula(rng, depth - 1)),
            6 => format!("sma({}, {w})", gen_formula(rng, depth - 1)),
            7 => format!("ema({}, {w})", gen_formula(rng, depth - 1)),
            8 => format!("std({}, {})", gen_formula(rng, depth - 1), w + 1),
            9 => format!("zscore({}, {})", gen_formula(rng, depth - 1), w + 1),
            10 => format!("rsi({}, {w})", gen_formula(rng, depth - 1)),
            11 => format!("ret({}, {w})", gen_formula(rng, depth - 1)),
            12 => format!("max({}, {w})", gen_formula(rng, depth - 1)),
            _ => format!("log(abs({}))", gen_formula(rng, depth - 1)),
        }
    }

    fn lookahead_corpus() -> Vec<String> {
        let mut v: Vec<String> = [
            "close",
            "close - open",
            "sma(close, 10)",
            "ema(close, 10)",
            "std(close, 10)",
            "sum(volume, 7)",
            "max(high, 9)",
            "min(low, 9)",
            "zscore(close, 15)",
            "rsi(close, 14)",
            "rsi(close)",
            "ret(close)",
            "ret(close, 4)",
            "shift(close, 3)",
            "shift(close, 0)",
            "abs(close - sma(close, 5))",
            "log(close / shift(close, 1))",
            "sqrt(volume)",
            "atr()",
            "atr(5) / close",
            "close > sma(close, 20)",
            "sma(close, 10) / close - 1",
            "zscore(ret(close, 2), 20)",
            "ema(rsi(close, 7), 5) - 50",
            "other(\"SPY\", \"close\")",
            "ret(other(\"SPY\", \"close\"), 2) - ret(close, 2)",
            "sma(other(\"SPY\", \"volume\"), 6)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for _ in 0..150 {
            let d = 1 + rng.below(4);
            let f = gen_formula(&mut rng, d);
            if compile(&f).is_ok() {
                v.push(f);
            }
        }
        v
    }

    fn perturb_after(bars: &mut [OHLCV], cut: usize) {
        for (k, b) in bars.iter_mut().enumerate().skip(cut + 1) {
            let f = 1.0 + 0.4 * ((k % 7) as f64 - 3.0);
            b.close = (b.close * f).max(0.5);
            b.open = b.close * 1.01;
            b.high = b.close * 1.2;
            b.low = b.close * 0.7;
            b.volume = b.volume * 9 + 5;
        }
    }

    #[test]
    fn no_lookahead_future_bars_never_change_earlier_values() {
        let bars = synthetic_bars(160);
        let mut ctx_bars = synthetic_bars(160);
        for b in &mut ctx_bars {
            b.symbol = Symbol("SPY".into());
            b.close *= 3.0;
            b.volume += 100;
        }
        // The context symbol is missing every 4th bar so forward fill is exercised too.
        let sparse = |v: &[OHLCV]| v.iter().enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, b)| b.clone()).collect::<Vec<_>>();
        let mut ctx = Context::new();
        ctx.insert("SPY".into(), sparse(&ctx_bars));
        let corpus = lookahead_corpus();
        assert!(corpus.len() > 100, "corpus of {}", corpus.len());
        for expr in &corpus {
            let c = compile(expr).unwrap();
            let base = evaluate(&c, &bars, &ctx).unwrap();
            for cut in [20usize, 61, 100, 140] {
                let mut p_bars = bars.clone();
                perturb_after(&mut p_bars, cut);
                let mut p_ctx_bars = ctx_bars.clone();
                perturb_after(&mut p_ctx_bars, cut);
                let mut p_ctx = Context::new();
                p_ctx.insert("SPY".into(), sparse(&p_ctx_bars));
                let other = evaluate(&c, &p_bars, &p_ctx).unwrap();
                for i in 0..=cut {
                    assert!(
                        (base[i].is_nan() && other[i].is_nan()) || base[i] == other[i],
                        "`{expr}` at bar {i} changed when bars after {cut} changed: {} vs {}",
                        base[i],
                        other[i]
                    );
                }
            }
        }
    }

    #[test]
    fn a_truncated_series_gives_the_same_prefix() {
        // Equivalent formulation of no-lookahead: computing on the first k bars equals the first
        // k values of computing on all bars.
        let bars = synthetic_bars(140);
        let mut ctx = Context::new();
        let mut spy = synthetic_bars(140);
        for b in &mut spy {
            b.symbol = Symbol("SPY".into());
            b.close += 7.0;
        }
        ctx.insert("SPY".into(), spy);
        for expr in lookahead_corpus() {
            let c = compile(&expr).unwrap();
            let full = evaluate(&c, &bars, &ctx).unwrap();
            for k in [30usize, 77, 139] {
                let part = evaluate(&c, &bars[..k], &ctx).unwrap();
                for i in 0..k {
                    assert!(
                        (full[i].is_nan() && part[i].is_nan()) || approx(full[i], part[i]),
                        "`{expr}` bar {i} of {k}: {} vs {}",
                        full[i],
                        part[i]
                    );
                }
            }
        }
    }

    // ── Robustness ───────────────────────────────────────────────────

    fn nasty_bars(rng: &mut Rng, n: usize) -> Vec<OHLCV> {
        let mut bars = synthetic_bars(n);
        let specials = [0.0, -1.0, 1e-300, 1e300, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0, 5e-324];
        for b in bars.iter_mut() {
            for slot in [&mut b.open, &mut b.high, &mut b.low, &mut b.close] {
                match rng.below(5) {
                    0 => *slot = specials[rng.below(specials.len())],
                    1 => *slot = rng.unit() * 200.0 - 50.0,
                    _ => {}
                }
            }
            if rng.below(6) == 0 {
                b.volume = [0u64, 1, u64::MAX][rng.below(3)];
            }
        }
        bars
    }

    #[test]
    fn fuzz_garbage_strings_and_hostile_series_never_panic_and_never_yield_infinity() {
        let pieces = [
            "close", "open", "volume", "sma(", "ema(", "rsi(", "atr(", "other(", "\"SPY\"", "'x'", ",", ")", "(", "+",
            "-", "*", "/", ">", "<", ">=", "<=", "=", "0", "1", "14", "500", "501", "1e999", "1e-999", ".", "..",
            " ", "\t", "é", "日本", "\"", "'", "log(", "sqrt(", "abs(", "shift(", "zscore(", "ret(", "std(", "9999999999999999999999",
            "-0", "--", "((((", "))))", "\u{0}", "\n",
        ];
        let mut rng = Rng(0xDEADBEEFCAFEF00D);
        let mut compiled_ok = 0;
        let mut spy = Context::new();
        for trial in 0..3000 {
            let len = 1 + rng.below(14);
            let expr: String = (0..len).map(|_| pieces[rng.below(pieces.len())]).collect();
            let Ok(c) = compile(&expr) else { continue };
            compiled_ok += 1;
            let n = rng.below(70);
            let bars = nasty_bars(&mut rng, n);
            let m = rng.below(40);
            spy.insert("SPY".into(), nasty_bars(&mut rng, m));
            spy.insert("X".into(), nasty_bars(&mut rng, 10));
            if let Ok(v) = evaluate(&c, &bars, &spy) {
                assert_eq!(v.len(), n, "trial {trial}: `{expr}`");
                assert!(v.iter().all(|x| !x.is_infinite()), "trial {trial}: `{expr}` produced infinity");
            }
        }
        assert!(compiled_ok > 50, "the fuzz alphabet should reach valid formulas ({compiled_ok})");
    }

    #[test]
    fn fuzz_random_bytes_never_panic_the_parser() {
        let mut rng = Rng(42);
        for _ in 0..4000 {
            let len = rng.below(60);
            let s: String = (0..len)
                .map(|_| char::from_u32(rng.below(0x2FF) as u32).unwrap_or('?'))
                .collect();
            let _ = compile(&s);
        }
        let huge = "(".repeat(100_000);
        assert!(compile(&huge).is_err());
        assert!(compile(&"-".repeat(100_000)).is_err());
    }

    #[test]
    fn fuzz_valid_formulas_on_hostile_series_stay_finite_or_nan() {
        let mut rng = Rng(7);
        let mut ctx = Context::new();
        for _ in 0..600 {
            let d = 1 + rng.below(5);
            let f = gen_formula(&mut rng, d);
            let Ok(c) = compile(&f) else { continue };
            let n = rng.below(90);
            let bars = nasty_bars(&mut rng, n);
            ctx.insert("SPY".into(), nasty_bars(&mut rng, 30));
            let v = evaluate(&c, &bars, &ctx).unwrap();
            assert_eq!(v.len(), n);
            assert!(v.iter().all(|x| !x.is_infinite()), "`{f}`");
        }
    }

    #[test]
    fn empty_series_gives_an_empty_result() {
        let c = compile("sma(close, 5) + 1").unwrap();
        assert!(evaluate(&c, &[], &no_ctx()).unwrap().is_empty());
        let c = compile("atr()").unwrap();
        assert!(evaluate(&c, &synthetic_bars(1), &no_ctx()).unwrap()[0].is_nan());
    }
}
