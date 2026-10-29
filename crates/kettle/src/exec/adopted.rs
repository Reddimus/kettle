//! Bounded, resumable reads of Linux's per-thread child lists.

use std::fs::{File, ReadDir};
use std::io::{self, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

const MAX_CHILDREN_PER_BATCH: usize = 1024;

pub(super) struct AdoptedChildren {
    task_path: PathBuf,
    tasks: Option<ReadDir>,
    list: Option<BufReader<File>>,
    token: Vec<u8>,
}

pub(super) struct Batch {
    pub children: Vec<i32>,
    pub complete: bool,
}

impl AdoptedChildren {
    pub fn new(task_path: &Path) -> Self {
        Self {
            task_path: task_path.to_owned(),
            tasks: None,
            list: None,
            token: Vec::new(),
        }
    }

    pub fn next_batch(
        &mut self,
        root: Option<u32>,
        own_session: i32,
        deadline: Instant,
        max_steps: usize,
        mut session: impl FnMut(i32) -> io::Result<i32>,
    ) -> io::Result<Batch> {
        let mut batch = Batch {
            children: Vec::new(),
            complete: false,
        };
        if Instant::now() >= deadline {
            return Ok(batch);
        }
        if self.tasks.is_none() {
            self.tasks = Some(std::fs::read_dir(&self.task_path)?);
        }
        for _ in 0..max_steps {
            if Instant::now() >= deadline || batch.children.len() >= MAX_CHILDREN_PER_BATCH {
                return Ok(batch);
            }
            if let Some(list) = &mut self.list {
                let mut byte = [0];
                let read = list.read(&mut byte)?;
                if read == 0 || byte[0].is_ascii_whitespace() {
                    if !self.token.is_empty() {
                        let pid: i32 = std::str::from_utf8(&self.token)
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .filter(|pid| *pid > 0)
                            .ok_or_else(|| {
                                io::Error::new(io::ErrorKind::InvalidData, "malformed child list")
                            })?;
                        self.token.clear();
                        if u32::try_from(pid).ok() != root {
                            let sid = session(pid)?;
                            if sid != -1 && sid != own_session {
                                batch.children.push(pid);
                            }
                        }
                    }
                    if read == 0 {
                        self.list = None;
                    }
                } else {
                    if !byte[0].is_ascii_digit() || self.token.len() >= 10 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "malformed child list",
                        ));
                    }
                    self.token.push(byte[0]);
                }
                continue;
            }
            let Some(task) = self.tasks.as_mut().unwrap().next() else {
                self.tasks = None;
                batch.complete = true;
                return Ok(batch);
            };
            let task = task?.path();
            match File::open(task.join("children")) {
                Ok(file) => self.list = Some(BufReader::new(file)),
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && matches!(std::fs::symlink_metadata(&task), Err(gone) if gone.kind() == io::ErrorKind::NotFound) =>
                    {}
                Err(error) => return Err(error),
            }
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn adopted_scan_bounds_work_and_resumes_past_live_children() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("thread")).unwrap();
        let pids: Vec<i32> = (1..=5000).collect();
        std::fs::write(
            root.path().join("thread/children"),
            pids.iter().map(|pid| format!("{pid} ")).collect::<String>(),
        )
        .unwrap();
        for max_steps in [128, 100_000] {
            let mut scan = AdoptedChildren::new(root.path());
            let mut seen = Vec::new();
            let mut passes = 0;
            loop {
                let mut queries = 0;
                let batch = scan
                    .next_batch(
                        Some(1),
                        7,
                        Instant::now() + Duration::from_secs(5),
                        max_steps,
                        |pid| {
                            queries += 1;
                            Ok(if pid == 2 { 7 } else { 8 })
                        },
                    )
                    .unwrap();
                assert!(queries <= max_steps, "unbounded session queries: {queries}");
                assert!(batch.children.len() <= 1024);
                seen.extend(batch.children);
                passes += 1;
                if batch.complete {
                    break;
                }
                assert!(passes < 1000);
            }
            assert!(passes > 1);
            assert_eq!(seen, pids[2..]);
        }
    }

    #[test]
    fn adopted_scan_expired_deadline_is_incomplete_even_with_no_children() {
        let root = tempfile::tempdir().unwrap();
        let mut scan = AdoptedChildren::new(root.path());
        let batch = scan
            .next_batch(None, 1, Instant::now(), 128, |_| {
                panic!("expired scan queried a PID")
            })
            .unwrap();
        assert!(
            !batch.complete,
            "an expired teardown scan cannot prove the tree empty"
        );
    }

    #[test]
    fn adopted_scan_bounds_empty_tasks_and_whitespace() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..300 {
            let path = root.path().join(i.to_string());
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("children"), " ".repeat(1000)).unwrap();
        }
        let mut scan = AdoptedChildren::new(root.path());
        let batch = scan
            .next_batch(
                None,
                1,
                Instant::now() + Duration::from_secs(5),
                128,
                |_| panic!(),
            )
            .unwrap();
        assert!(!batch.complete);
    }
}
