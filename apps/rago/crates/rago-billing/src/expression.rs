//! Custom-aggregation expressions — a port of Lago's `lago-expression`
//! (`expression-core`, MIT) grammar and evaluator on `rust_decimal`.
//!
//! Grammar (same as `grammar.pest`):
//! * atoms: decimals (`12`, `3.5`), strings (`'text'`), `event.code`,
//!   `event.timestamp`, `event.properties.<name>`, `( expr )`, functions;
//! * operators: `+ -` < `* /` < unary `-` (left-associative);
//! * functions (case: lower, UPPER or Capitalized): `round(x[, digits])`
//!   (half-up), `ceil(x[, digits])`, `floor(x[, digits])`, `concat(a, ...)`,
//!   `least(a, ...)`, `greatest(a, ...)`.
//!
//! String property values that parse as numbers are numbers (as upstream);
//! arithmetic on a non-numeric string is an error.

use std::fmt;
use std::str::FromStr;

use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::Value;

/// An evaluated value.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprValue {
    Number(Decimal),
    String(String),
}

impl fmt::Display for ExprValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprValue::Number(d) => write!(f, "{}", d.normalize()),
            ExprValue::String(s) => f.write_str(s),
        }
    }
}

/// Parse / evaluation failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExprError {
    #[error("failed to parse expression: {0}")]
    Parse(String),
    #[error("missing variable: {0}")]
    MissingVariable(String),
    #[error("expected a decimal value")]
    ExpectedDecimal,
    #[error("division by zero")]
    DivisionByZero,
    #[error("function requires at least one argument")]
    EmptyArgumentList,
}

#[derive(Debug, Clone, PartialEq)]
enum Func {
    Round,
    Ceil,
    Floor,
    Concat,
    Least,
    Greatest,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Number(Decimal),
    Str(String),
    Code,
    Timestamp,
    Property(String),
    Neg(Box<Expr>),
    Bin(Box<Expr>, char, Box<Expr>),
    Call(Func, Vec<Expr>),
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn err<T>(&self, msg: &str) -> Result<T, ExprError> {
        Err(ExprError::Parse(format!("{msg} at position {}", self.i)))
    }

    // expr = term (("+"|"-") term)*
    fn expr(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.term()?;
        loop {
            match self.peek() {
                Some(c @ (b'+' | b'-')) => {
                    self.i += 1;
                    let rhs = self.term()?;
                    lhs = Expr::Bin(Box::new(lhs), c as char, Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }
    // term = unary (("*"|"/") unary)*
    fn term(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.unary()?;
        loop {
            match self.peek() {
                Some(c @ (b'*' | b'/')) => {
                    self.i += 1;
                    let rhs = self.unary()?;
                    lhs = Expr::Bin(Box::new(lhs), c as char, Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }
    // unary = "-"? primary
    fn unary(&mut self) -> Result<Expr, ExprError> {
        if self.eat(b'-') {
            return Ok(Expr::Neg(Box::new(self.primary()?)));
        }
        self.primary()
    }
    fn ident(&mut self) -> String {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }
    fn primary(&mut self) -> Result<Expr, ExprError> {
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                let e = self.expr()?;
                if !self.eat(b')') {
                    return self.err("expected ')'");
                }
                Ok(e)
            }
            Some(b'\'') => {
                self.i += 1;
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'\'' {
                    self.i += 1;
                }
                if self.i >= self.s.len() {
                    return self.err("unterminated string");
                }
                let v = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                self.i += 1;
                Ok(Expr::Str(v))
            }
            Some(c) if c.is_ascii_digit() => {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                if self.i + 1 < self.s.len() && self.s[self.i] == b'.' && self.s[self.i + 1].is_ascii_digit() {
                    self.i += 1;
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                }
                let txt = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                Decimal::from_str(&txt).map(Expr::Number).or_else(|_| self.err("invalid decimal"))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let name = self.ident();
                if name == "event" {
                    return self.variable();
                }
                let func = match name.as_str() {
                    "round" | "ROUND" | "Round" => Func::Round,
                    "ceil" | "CEIL" | "Ceil" => Func::Ceil,
                    "floor" | "FLOOR" | "Floor" => Func::Floor,
                    "concat" | "CONCAT" | "Concat" => Func::Concat,
                    "least" | "LEAST" | "Least" => Func::Least,
                    "greatest" | "GREATEST" | "Greatest" => Func::Greatest,
                    _ => return self.err(&format!("unknown identifier `{name}`")),
                };
                if !self.eat(b'(') {
                    return self.err("expected '('");
                }
                let mut args = vec![self.expr()?];
                while self.eat(b',') {
                    args.push(self.expr()?);
                }
                if !self.eat(b')') {
                    return self.err("expected ')'");
                }
                let arity_ok = match func {
                    Func::Round | Func::Ceil | Func::Floor => args.len() <= 2,
                    _ => true,
                };
                if !arity_ok {
                    return self.err("too many arguments");
                }
                Ok(Expr::Call(func, args))
            }
            _ => self.err("expected a value"),
        }
    }
    // after `event`: ".code" | ".timestamp" | ".properties.<name>" (no spaces)
    fn variable(&mut self) -> Result<Expr, ExprError> {
        if self.s.get(self.i) != Some(&b'.') {
            return self.err("expected '.' after event");
        }
        self.i += 1;
        match self.ident().as_str() {
            "code" => Ok(Expr::Code),
            "timestamp" => Ok(Expr::Timestamp),
            "properties" => {
                if self.s.get(self.i) != Some(&b'.') {
                    return self.err("expected '.' after properties");
                }
                self.i += 1;
                if !self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
                    return self.err("invalid property name");
                }
                Ok(Expr::Property(self.ident()))
            }
            other => self.err(&format!("unknown event attribute `{other}`")),
        }
    }
}

/// A parsed expression, ready to evaluate against events.
#[derive(Debug, Clone, PartialEq)]
pub struct Expression(Expr);

impl Expression {
    /// Parse `input` with the Lago expression grammar.
    pub fn parse(input: &str) -> Result<Self, ExprError> {
        let mut p = Parser { s: input.as_bytes(), i: 0 };
        let e = p.expr()?;
        p.ws();
        if p.i != p.s.len() {
            return p.err("unexpected trailing input");
        }
        Ok(Expression(e))
    }

    /// Evaluate against an event given as `{ code, timestamp, properties }`.
    pub fn evaluate(&self, event: &Value) -> Result<ExprValue, ExprError> {
        eval(&self.0, event)
    }
}

fn json_to_value(v: &Value) -> Option<ExprValue> {
    match v {
        Value::Number(n) => Decimal::from_str(&n.to_string())
            .or_else(|_| Decimal::from_scientific(&n.to_string()))
            .ok()
            .map(ExprValue::Number),
        Value::String(s) => Some(match Decimal::from_str(s.trim()) {
            Ok(d) => ExprValue::Number(d),
            Err(_) => ExprValue::String(s.clone()),
        }),
        Value::Bool(b) => Some(ExprValue::String(b.to_string())),
        _ => None,
    }
}

fn num(v: ExprValue) -> Result<Decimal, ExprError> {
    match v {
        ExprValue::Number(d) => Ok(d),
        ExprValue::String(_) => Err(ExprError::ExpectedDecimal),
    }
}

fn round_to(d: Decimal, digits: i64, strategy: RoundingStrategy) -> Decimal {
    if digits >= 0 {
        d.round_dp_with_strategy(digits.min(28) as u32, strategy)
    } else {
        let factor = Decimal::from(10i64.pow(digits.unsigned_abs().min(18) as u32));
        (d / factor).round_dp_with_strategy(0, strategy) * factor
    }
}

fn eval(e: &Expr, event: &Value) -> Result<ExprValue, ExprError> {
    Ok(match e {
        Expr::Number(d) => ExprValue::Number(*d),
        Expr::Str(s) => ExprValue::String(s.clone()),
        Expr::Code => ExprValue::String(event.get("code").and_then(Value::as_str).unwrap_or_default().to_string()),
        Expr::Timestamp => event
            .get("timestamp")
            .and_then(json_to_value)
            .ok_or_else(|| ExprError::MissingVariable("timestamp".into()))?,
        Expr::Property(name) => event
            .get("properties")
            .and_then(|p| p.get(name))
            .and_then(json_to_value)
            .ok_or_else(|| ExprError::MissingVariable(name.clone()))?,
        Expr::Neg(inner) => ExprValue::Number(-num(eval(inner, event)?)?),
        Expr::Bin(l, op, r) => {
            let a = num(eval(l, event)?)?;
            let b = num(eval(r, event)?)?;
            ExprValue::Number(match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                _ => {
                    if b.is_zero() {
                        return Err(ExprError::DivisionByZero);
                    }
                    a / b
                }
            })
        }
        Expr::Call(f, args) => match f {
            Func::Concat => ExprValue::String(
                args.iter()
                    .map(|a| eval(a, event).map(|v| v.to_string()))
                    .collect::<Result<Vec<_>, _>>()?
                    .concat(),
            ),
            Func::Least | Func::Greatest => {
                let vals = args
                    .iter()
                    .map(|a| eval(a, event).and_then(num))
                    .collect::<Result<Vec<_>, _>>()?;
                let v = if *f == Func::Least { vals.into_iter().min() } else { vals.into_iter().max() };
                ExprValue::Number(v.ok_or(ExprError::EmptyArgumentList)?)
            }
            Func::Round | Func::Ceil | Func::Floor => {
                let x = num(eval(&args[0], event)?)?;
                let digits = match args.get(1) {
                    Some(d) => {
                        use rust_decimal::prelude::ToPrimitive;
                        num(eval(d, event)?)?.trunc().to_i64().ok_or(ExprError::ExpectedDecimal)?
                    }
                    None => 0,
                };
                let strategy = match f {
                    Func::Round => RoundingStrategy::MidpointAwayFromZero,
                    Func::Ceil => RoundingStrategy::ToPositiveInfinity,
                    _ => RoundingStrategy::ToNegativeInfinity,
                };
                ExprValue::Number(round_to(x, digits, strategy))
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(expr: &str, event: Value) -> Result<String, ExprError> {
        Expression::parse(expr)?.evaluate(&event).map(|v| v.to_string())
    }

    #[test]
    fn arithmetic_precedence_and_functions() {
        let ev = json!({ "code": "c", "timestamp": 1, "properties": { "a": "2.5", "b": 4, "s": "x" } });
        assert_eq!(run("event.properties.a * event.properties.b + 1", ev.clone()).unwrap(), "11");
        assert_eq!(run("-(1 + 2) * 3", ev.clone()).unwrap(), "-9");
        assert_eq!(run("round(10 / 3, 2)", ev.clone()).unwrap(), "3.33");
        assert_eq!(run("ceil(1.21, 1)", ev.clone()).unwrap(), "1.3");
        assert_eq!(run("FLOOR(1.99)", ev.clone()).unwrap(), "1");
        assert_eq!(run("concat(event.code, '-', event.properties.s)", ev.clone()).unwrap(), "c-x");
        assert_eq!(run("least(3, event.properties.b, 7)", ev.clone()).unwrap(), "3");
        assert_eq!(run("greatest(3, event.properties.b, 7)", ev.clone()).unwrap(), "7");
        assert!(matches!(run("event.properties.missing", ev.clone()), Err(ExprError::MissingVariable(_))));
        assert!(matches!(run("event.properties.s + 1", ev.clone()), Err(ExprError::ExpectedDecimal)));
        assert!(matches!(run("1 +", ev), Err(ExprError::Parse(_))));
    }
}
