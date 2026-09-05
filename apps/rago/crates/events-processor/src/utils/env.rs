//! Port of `utils/env.go`.

use std::env;

/// `GetEnvAsInt`.
pub fn get_env_as_int(
    key: &str,
    default_value: i64,
) -> std::result::Result<i64, std::num::ParseIntError> {
    match env::var(key) {
        Ok(v) if v.is_empty() => Ok(default_value),
        Ok(v) => v.parse::<i64>(),
        Err(_) => Ok(default_value),
    }
}

/// `ParseBrokersEnv`.
pub fn parse_brokers_env(brokers_str: &str) -> Vec<String> {
    if brokers_str.is_empty() {
        return Vec::new();
    }
    brokers_str
        .split(',')
        .map(|b| b.trim().to_string())
        .collect()
}

/// `GetEnvAsBool` — Go uses `strconv.ParseBool`, which accepts
/// 1/t/T/TRUE/true/True and 0/f/F/FALSE/false/False.
pub fn get_env_as_bool(key: &str, default_value: bool) -> bool {
    match env::var(key) {
        Ok(v) => parse_go_bool(&v).unwrap_or(default_value),
        Err(_) => default_value,
    }
}

/// `GetEnvOrDefault`.
pub fn get_env_or_default(key: &str, default_value: &str) -> String {
    match env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => default_value.to_string(),
    }
}

fn parse_go_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}
