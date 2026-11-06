use std::time::{Duration, Instant};

use crate::Error;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(4);
const DRAIN_CHUNKS_MAX: usize = 4096;

/// Raw formats read by the default file/image paste branches, before decoding.
#[derive(Clone, Copy)]
pub(super) enum PasteFormat {
	Image,
	Files,
}

pub(super) fn read_paste<T>(
	format: PasteFormat,
	read: impl FnOnce(Option<crate::common::ReadBounds>) -> Result<T, Error>,
) -> Result<T, Error> {
	let max_bytes = match format {
		// Match Kettle's existing pasted-image source limit, plus one sentinel byte.
		PasteFormat::Image => 256 * 1024 * 1024 + 1,
		PasteFormat::Files => 1024 * 1024 + 1,
	};
	read(Some(crate::common::ReadBounds {
		max_bytes,
		deadline: Instant::now() + Duration::from_secs(2),
	}))
}

#[derive(Default)]
pub(super) struct IncrRead {
	pub(super) data: Vec<u8>,
	pub(super) mismatch: bool,
	drain: Option<(Instant, usize)>,
	bounds: Option<(usize, Instant)>,
}

impl IncrRead {
	pub(super) fn bounded(max_bytes: usize, deadline: Instant) -> Self {
		Self { bounds: Some((max_bytes, deadline)), ..Self::default() }
	}

	pub(super) fn remaining_bounds(&self) -> Option<crate::common::ReadBounds> {
		self.bounds.map(|(max_bytes, deadline)| crate::common::ReadBounds {
			max_bytes: max_bytes.saturating_sub(self.data.len()),
			deadline,
		})
	}

	pub(super) fn push(
		&mut self,
		matches: bool,
		value: Vec<u8>,
		now: Instant,
	) -> Result<bool, Error> {
		if let Some((max_bytes, deadline)) = self.bounds {
			if now >= deadline {
				return Err(Error::Timeout);
			}
			if value.len() > max_bytes.saturating_sub(self.data.len()) {
				return Err(Error::LimitExceeded);
			}
		}
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
			if self.bounds.is_some() {
				self.data
					.try_reserve_exact(value.len())
					.map_err(|_| Error::unknown("clipboard allocation failed"))?;
			}
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

#[cfg(test)]
mod bounded_tests {
	use super::*;

	#[test]
	fn bounded_incr_rejects_oversized_correctly_typed_segments() {
		let start = Instant::now();
		let mut incr = IncrRead::bounded(4, start + Duration::from_secs(2));
		let events = [(start, vec![1, 2, 3, 4]), (start, vec![5])];
		let mut events = events.into_iter();
		let (now, value) = events.next().unwrap();
		assert!(!incr.push(true, value, now).unwrap());
		assert_eq!(incr.remaining_bounds().unwrap().max_bytes, 0);
		let (now, value) = events.next().unwrap();
		assert!(
			matches!(incr.push(true, value, now), Err(Error::LimitExceeded)),
			"correctly typed INCR exceeded its byte budget"
		);
		assert_eq!(incr.data, [1, 2, 3, 4], "rejected chunk must not be appended");
	}

	#[test]
	fn bounded_incr_unterminated_correctly_typed_sender_cannot_renew_deadline() {
		let start = Instant::now();
		let mut incr = IncrRead::bounded(1024, start + Duration::from_secs(2));
		let events = (0..=4).map(|tick| (start + Duration::from_millis(tick * 500), vec![1]));
		for (index, (now, value)) in events.enumerate() {
			let result = incr.push(true, value, now);
			if index < 4 {
				assert!(!result.unwrap());
			} else {
				assert!(
					matches!(result, Err(Error::Timeout)),
					"correctly typed INCR exceeded its absolute deadline"
				);
			}
		}
	}
}

#[cfg(test)]
mod paste_tests {
	use super::*;
	// Drive the acquisition used by the default paste branches before PNG/URI decoding.
	// The owner sends correctly typed INCR chunks forever, without an end marker.
	fn unterminated_paste(format: PasteFormat, slow: bool) -> Error {
		use std::time::{Duration, Instant};
		let start = Instant::now();
		read_paste(format, |bounds| -> Result<(), Error> {
			let mut transfer = match bounds {
				Some(bounds) => IncrRead::bounded(bounds.max_bytes, bounds.deadline),
				None => IncrRead::default(),
			};
			if slow {
				for tick in 0..=4 {
					transfer.push(true, vec![1], start + Duration::from_secs(tick))?;
				}
			} else {
				// Reach the exact ceiling, then refuse the next chunk before appending.
				let limit = match format {
					PasteFormat::Image => 256 * 1024 * 1024 + 1,
					PasteFormat::Files => 1024 * 1024 + 1,
				};
				let mut sent = 0;
				while sent < limit {
					let count = (limit - sent).min(1024 * 1024);
					transfer.push(true, vec![1; count], start)?;
					sent += count;
				}
				transfer.push(true, vec![1], start)?;
			}
			panic!("default paste accepted an unterminated INCR transfer");
		})
		.unwrap_err()
	}

	#[test]
	fn default_paste_refuses_unterminated_image_png_and_uri_list_incr() {
		for format in [PasteFormat::Image, PasteFormat::Files] {
			assert!(matches!(unterminated_paste(format, true), Error::Timeout));
			assert!(matches!(unterminated_paste(format, false), Error::LimitExceeded));
		}
	}
}
