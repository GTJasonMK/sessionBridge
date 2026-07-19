use std::time::Duration;

use super::{InvitationError, Result};

pub fn parse_duration(input: &str) -> Result<Duration> {
    let input = input.trim();
    if input.is_empty() {
        return Err(InvitationError::InvalidDuration(
            "duration cannot be empty".to_owned(),
        ));
    }

    let bytes = input.as_bytes();
    let mut index = 0;
    let mut total_seconds = 0_u64;

    while index < bytes.len() {
        let number_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if number_start == index || index >= bytes.len() {
            return Err(InvitationError::InvalidDuration(format!(
                "expected an integer followed by s, m, h, d, or w near byte {number_start}"
            )));
        }

        let amount = input[number_start..index].parse::<u64>().map_err(|_| {
            InvitationError::InvalidDuration("duration component is too large".to_owned())
        })?;
        let multiplier = match bytes[index] {
            b's' => 1,
            b'm' => 60,
            b'h' => 60 * 60,
            b'd' => 24 * 60 * 60,
            b'w' => 7 * 24 * 60 * 60,
            unit => {
                return Err(InvitationError::InvalidDuration(format!(
                    "unknown duration unit {:?}",
                    char::from(unit)
                )));
            }
        };
        index += 1;

        let component = amount.checked_mul(multiplier).ok_or_else(|| {
            InvitationError::InvalidDuration("duration overflows u64 seconds".to_owned())
        })?;
        total_seconds = total_seconds.checked_add(component).ok_or_else(|| {
            InvitationError::InvalidDuration("duration overflows u64 seconds".to_owned())
        })?;
    }

    if total_seconds == 0 {
        return Err(InvitationError::InvalidDuration(
            "duration must be greater than zero".to_owned(),
        ));
    }

    Ok(Duration::from_secs(total_seconds))
}
