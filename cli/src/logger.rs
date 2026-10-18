use log::{LevelFilter, Log, Metadata, Record};

const ENV: &str = "TYPESAFE_LOG_LEVEL";
const LEVELS: &str = "debug, info, warn, error, off";

/// Writes SDK log lines to stderr, `[typesafe-sdk] level message`.
struct Stderr;

impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.target().starts_with("typesafe_sdk")
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "[typesafe-sdk] {} {}",
                record.level().as_str().to_lowercase(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

fn parse_level(value: &str) -> Result<LevelFilter, String> {
    match value.trim().to_lowercase().as_str() {
        "debug" => Ok(LevelFilter::Debug),
        "info" => Ok(LevelFilter::Info),
        "warn" => Ok(LevelFilter::Warn),
        "error" => Ok(LevelFilter::Error),
        "off" => Ok(LevelFilter::Off),
        _ => Err(format!(
            "Invalid log level \"{value}\" from {ENV}. Expected one of: {LEVELS}."
        )),
    }
}

/// Install the stderr logger at the level in `TYPESAFE_LOG_LEVEL` (default `warn`).
pub fn init() -> Result<(), String> {
    let level = match std::env::var(ENV) {
        Ok(v) if !v.trim().is_empty() => parse_level(&v)?,
        _ => LevelFilter::Warn,
    };
    log::set_logger(&Stderr).map_err(|e| e.to_string())?;
    log::set_max_level(level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels() {
        assert_eq!(parse_level(" INFO ").unwrap(), LevelFilter::Info);
        assert_eq!(parse_level("off").unwrap(), LevelFilter::Off);
        assert!(parse_level("loud").unwrap_err().contains("Expected one of"));
    }
}
