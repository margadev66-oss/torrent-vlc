use anyhow::{Result, anyhow, bail};
use std::time::Duration;

pub fn parse_byte_size(input: &str) -> Result<u64> {
    let value = input.trim();
    if value.is_empty() {
        bail!("size cannot be empty");
    }

    let split_at = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split_at);
    let number: f64 = number
        .parse()
        .map_err(|_| anyhow!("invalid size number in {input:?}"))?;
    if !number.is_finite() || number < 0.0 {
        bail!("size must be a finite non-negative number: {input:?}");
    }

    let multiplier = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1_000.0,
        "ki" | "kib" => 1_024.0,
        "m" | "mb" => 1_000_000.0,
        "mi" | "mib" => 1_048_576.0,
        "g" | "gb" => 1_000_000_000.0,
        "gi" | "gib" => 1_073_741_824.0,
        "t" | "tb" => 1_000_000_000_000.0,
        "ti" | "tib" => 1_099_511_627_776.0,
        other => bail!("unknown size suffix {other:?} in {input:?}"),
    };

    let bytes = number * multiplier;
    if bytes > u64::MAX as f64 {
        bail!("size is too large: {input:?}");
    }
    Ok(bytes.round() as u64)
}

pub fn parse_duration(input: &str) -> Result<Duration> {
    let value = input.trim();
    if value.is_empty() {
        bail!("duration cannot be empty");
    }

    let split_at = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split_at);
    let number: f64 = number
        .parse()
        .map_err(|_| anyhow!("invalid duration number in {input:?}"))?;
    if !number.is_finite() || number < 0.0 {
        bail!("duration must be a finite non-negative number: {input:?}");
    }

    let seconds = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "s" => number,
        "ms" => number / 1_000.0,
        "m" | "min" => number * 60.0,
        "h" => number * 3_600.0,
        other => bail!("unknown duration suffix {other:?} in {input:?}"),
    };
    if !seconds.is_finite() || seconds > Duration::MAX.as_secs_f64() {
        bail!("duration is too large: {input:?}");
    }
    Ok(Duration::from_secs_f64(seconds))
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_and_decimal_sizes() {
        assert_eq!(parse_byte_size("64M").unwrap(), 64_000_000);
        assert_eq!(parse_byte_size("64MiB").unwrap(), 64 * 1024 * 1024);
        assert_eq!(parse_byte_size("1.5G").unwrap(), 1_500_000_000);
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("120s").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1.5m").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
    }
}
