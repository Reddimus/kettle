//! Bounded command outcomes anchored to primary-grid prompt rows.
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandMark {
    pub row: u64,
    pub exit_code: i32,
}

#[derive(Default)]
pub(super) struct CommandMarks {
    pending: Option<u64>,
    marks: VecDeque<CommandMark>,
}

impl CommandMarks {
    pub(super) fn begin(&mut self, row: Option<u64>) {
        if self.pending.is_none() {
            self.pending = row;
        }
    }

    pub(super) fn end(&mut self, code: Option<i32>) {
        if let Some(row) = self.pending.take()
            && let Some(exit_code) = code
        {
            if self.marks.len() == 2048 {
                self.marks.pop_front();
            }
            self.marks.push_back(CommandMark { row, exit_code });
        }
    }

    pub(super) fn retained(&mut self, origin: u64, end: u64) -> Vec<CommandMark> {
        self.marks.retain(|mark| (origin..end).contains(&mark.row));
        self.marks.iter().copied().collect()
    }

    pub(super) fn cancel_pending(&mut self) {
        self.pending = None;
    }

    pub(super) fn clear(&mut self) {
        self.pending = None;
        self.marks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_status_requires_a_command_and_known_exit_code() {
        let mut marks = CommandMarks::default();
        marks.end(Some(1));
        marks.begin(Some(10));
        marks.begin(Some(11)); // duplicate C from a compound bash command
        marks.end(Some(0));
        marks.end(Some(9)); // duplicate D cannot change a completed command
        marks.begin(Some(12));
        marks.end(Some(-1));
        marks.begin(Some(13));
        marks.end(None);
        assert_eq!(
            marks.retained(0, 20),
            vec![
                CommandMark {
                    row: 10,
                    exit_code: 0
                },
                CommandMark {
                    row: 12,
                    exit_code: -1
                },
            ]
        );
        assert_eq!(
            marks.retained(11, 20),
            vec![CommandMark {
                row: 12,
                exit_code: -1
            }]
        );
        marks.clear();
        assert!(marks.retained(0, 20).is_empty());
    }

    #[test]
    fn outcomes_are_bounded_and_reflow_cancels_pending_commands() {
        let mut marks = CommandMarks::default();
        for row in 0..3000 {
            marks.begin(Some(row));
            marks.end(Some(2));
        }
        let retained = marks.retained(0, 4000);
        assert_eq!(retained.len(), 2048);
        assert_eq!(retained[0].row, 952);
        marks.begin(Some(3001));
        marks.clear();
        marks.end(Some(0));
        assert!(marks.retained(0, 4000).is_empty());
    }
}
