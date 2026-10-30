use std::time::{Duration, Instant};

use crate::Error;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(4);
const DRAIN_CHUNKS_MAX: usize = 4096;

#[derive(Default)]
pub(super) struct IncrRead {
	pub(super) data: Vec<u8>,
	pub(super) mismatch: bool,
	drain: Option<(Instant, usize)>,
}

impl IncrRead {
	pub(super) fn push(
		&mut self,
		matches: bool,
		value: Vec<u8>,
		now: Instant,
	) -> Result<bool, Error> {
		if !matches {
			self.mismatch = true;
		}
		if self.mismatch {
			let (deadline, remaining) =
				self.drain.get_or_insert((now + DRAIN_TIMEOUT, DRAIN_CHUNKS_MAX));
			if now >= *deadline || *remaining == 0 {
				return Err(Error::unknown("rejected INCR transfer exceeded its drain budget"));
			}
			*remaining -= 1;
		}
		if value.is_empty() {
			return Ok(true);
		}
		if !self.mismatch {
			self.data.extend(value);
		}
		Ok(false)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rejected_incr_sender_cannot_renew_the_drain_forever() {
		let mut incr = IncrRead::default();
		let start = Instant::now();
		assert!(!incr.push(false, vec![1], start).unwrap());
		for _ in 1..DRAIN_CHUNKS_MAX {
			assert!(!incr.push(false, vec![1], start).unwrap());
		}
		assert!(
			incr.push(false, vec![1], start).is_err(),
			"unterminated rejected INCR exceeded its chunk budget"
		);
		assert!(incr.data.is_empty());
	}

	#[test]
	fn rejected_incr_deadline_is_not_extended_by_segments() {
		let mut incr = IncrRead::default();
		let start = Instant::now();
		assert!(!incr.push(false, vec![1], start).unwrap());
		assert!(!incr.push(false, vec![1], start + DRAIN_TIMEOUT / 2).unwrap());
		assert!(
			incr.push(true, vec![1], start + DRAIN_TIMEOUT).is_err(),
			"rejected INCR exceeded its total deadline"
		);
	}

	#[test]
	fn finite_mismatch_is_drained_and_valid_data_is_preserved() {
		let mut incr = IncrRead::default();
		let now = Instant::now();
		assert!(!incr.push(true, vec![1, 2], now).unwrap());
		assert!(incr.push(true, vec![], now).unwrap());
		assert_eq!(incr.data, [1, 2]);
		assert!(!incr.mismatch);

		assert!(!incr.push(false, vec![3], now).unwrap());
		assert!(incr.push(false, vec![], now).unwrap());
		assert!(incr.mismatch);
		assert_eq!(incr.data, [1, 2]);
	}
}
