// Copyright (C) 2022 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

#![deny(clippy::disallowed_methods)]

mod checklist;
mod coolid;

pub mod fs;
pub mod io;
mod kill_switch;
pub mod metrics;
pub mod net;
mod progress;
pub mod rand;
pub mod rendezvous_hasher;
pub mod runtimes;
pub mod uri;

use std::env;
use std::fmt::Debug;
use std::ops::{Range, RangeInclusive};
use std::str::FromStr;

pub use checklist::{
    print_checklist, run_checklist, ChecklistError, BLUE_COLOR, GREEN_COLOR, RED_COLOR,
};
pub use coolid::new_coolid;
pub use kill_switch::KillSwitch;
pub use progress::{Progress, ProtectedZoneGuard};
use tracing::{error, info};

pub fn chunk_range(range: Range<usize>, chunk_size: usize) -> impl Iterator<Item = Range<usize>> {
    range.clone().step_by(chunk_size).map(move |block_start| {
        let block_end = (block_start + chunk_size).min(range.end);
        block_start..block_end
    })
}

pub fn into_u64_range(range: Range<usize>) -> Range<u64> {
    range.start as u64..range.end as u64
}

pub fn setup_logging_for_tests() {
    let _ = env_logger::builder().format_timestamp(None).try_init();
}

pub fn split_file(split_id: &str) -> String {
    format!("{}.split", split_id)
}

pub fn get_from_env<T: FromStr + Debug>(key: &str, default_value: T) -> T {
    if let Ok(value_str) = std::env::var(key) {
        if let Ok(value) = T::from_str(&value_str) {
            info!(value=?value, "Setting `{}` from environment", key);
            return value;
        } else {
            error!(value_str=%value_str, "Failed to parse `{}` from environment", key);
        }
    }
    info!(value=?default_value, "Setting `{}` from default", key);
    default_value
}

pub fn truncate_str(text: &str, max_len: usize) -> &str {
    if max_len > text.len() {
        return text;
    }

    let mut truncation_index = max_len;
    while !text.is_char_boundary(truncation_index) {
        truncation_index -= 1;
    }
    &text[..truncation_index]
}

/// Extracts time range from optional start and end timestamps.
pub fn extract_time_range(
    start_timestamp_opt: Option<i64>,
    end_timestamp_opt: Option<i64>,
) -> Option<Range<i64>> {
    match (start_timestamp_opt, end_timestamp_opt) {
        (Some(start_timestamp), Some(end_timestamp)) => Some(Range {
            start: start_timestamp,
            end: end_timestamp,
        }),
        (_, Some(end_timestamp)) => Some(Range {
            start: i64::MIN,
            end: end_timestamp,
        }),
        (Some(start_timestamp), _) => Some(Range {
            start: start_timestamp,
            end: i64::MAX,
        }),
        _ => None,
    }
}

/// Takes 2 intervals and returns true iff their intersection is empty
pub fn is_disjoint(left: &Range<i64>, right: &RangeInclusive<i64>) -> bool {
    left.end <= *right.start() || *right.end() < left.start
}

/// For use with the `skip_serializing_if` serde attribute.
pub fn is_false(value: &bool) -> bool {
    !*value
}

pub fn no_color() -> bool {
    matches!(env::var("NO_COLOR"), Ok(value) if !value.is_empty())
}

#[macro_export]
macro_rules! ignore_error_kind {
    ($kind:path, $expr:expr) => {
        match $expr {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == $kind => Ok(()),
            Err(error) => Err(error),
        }
    };
}

pub struct PrettySample<'a, T>(&'a [T], usize);

impl<'a, T> PrettySample<'a, T> {
    pub fn new(slice: &'a [T], sample_size: usize) -> Self {
        Self(slice, sample_size)
    }
}

impl<T> Debug for PrettySample<'_, T>
where T: Debug
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "[")?;
        for (i, item) in self.0.iter().enumerate() {
            if i == self.1 {
                write!(formatter, ", and {} more", self.0.len() - i)?;
                break;
            }
            if i > 0 {
                write!(formatter, ", ")?;
            }
            write!(formatter, "{:?}", item)?;
        }
        write!(formatter, "]")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn test_get_from_env() {
        const TEST_KEY: &str = "TEST_KEY";
        assert_eq!(super::get_from_env(TEST_KEY, 10), 10);
        std::env::set_var(TEST_KEY, "15");
        assert_eq!(super::get_from_env(TEST_KEY, 10), 15);
        std::env::set_var(TEST_KEY, "1invalidnumber");
        assert_eq!(super::get_from_env(TEST_KEY, 10), 10);
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("", 0), "");
        assert_eq!(truncate_str("", 3), "");
        assert_eq!(truncate_str("hello", 0), "");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("hello", 6), "hello");
        assert_eq!(truncate_str("hello-world", 5), "hello");
        assert_eq!(truncate_str("hello-world", 6), "hello-");
        assert_eq!(truncate_str("hello🧑‍🔬world", 6), "hello");
        assert_eq!(truncate_str("hello🧑‍🔬world", 7), "hello");
    }

    #[test]
    fn test_ignore_io_error_macro() {
        ignore_error_kind!(
            ErrorKind::NotFound,
            std::fs::remove_file("file-does-not-exist")
        )
        .unwrap();
    }

    #[test]
    fn test_pretty_sample() {
        let pretty_sample = PrettySample::<'_, usize>::new(&[], 2);
        assert_eq!(format!("{:?}", pretty_sample), "[]");

        let pretty_sample = PrettySample::new(&[1], 2);
        assert_eq!(format!("{:?}", pretty_sample), "[1]");

        let pretty_sample = PrettySample::new(&[1, 2], 2);
        assert_eq!(format!("{:?}", pretty_sample), "[1, 2]");

        let pretty_sample = PrettySample::new(&[1, 2, 3], 2);
        assert_eq!(format!("{:?}", pretty_sample), "[1, 2, and 1 more]");

        let pretty_sample = PrettySample::new(&[1, 2, 3, 4], 2);
        assert_eq!(format!("{:?}", pretty_sample), "[1, 2, and 2 more]");
    }
}
