use super::super::uri_reader;
use super::*;
use std::collections::VecDeque;
use std::path::PathBuf;
use wayland_client::protocol::wl_data_offer;

pub(crate) const URI_LIST: &str = "text/uri-list";
const MAX_PENDING: usize = 2;

pub(crate) enum FileUpdate {
    Start {
        id: u64,
        window: u64,
    },
    Ready {
        id: u64,
        paths: Result<Vec<PathBuf>, ()>,
    },
}

struct Pending {
    id: u64,
    offer: wl_data_offer::WlDataOffer,
    job: uri_reader::Job,
    sent: bool,
}

#[derive(Default)]
pub(super) struct Files {
    pending: Vec<Pending>,
    updates: VecDeque<FileUpdate>,
    next: u64,
}

impl Files {
    pub(super) fn start(
        &mut self,
        offer: wl_data_offer::WlDataOffer,
        window: u64,
        wake: kettle_core::Waker,
    ) {
        let Some(id) = self
            .next
            .checked_add(1)
            .filter(|_| self.pending.len() < MAX_PENDING)
        else {
            offer.destroy();
            return;
        };
        let Ok((job, writer)) = uri_reader::Job::new(wake) else {
            offer.destroy();
            return;
        };
        offer.receive(URI_LIST.into(), writer.as_fd());
        drop(writer);
        self.next = id;
        self.pending.push(Pending {
            id,
            offer,
            job,
            sent: false,
        });
        self.updates.push_back(FileUpdate::Start { id, window });
    }

    pub(super) fn poll(&mut self) {
        for pending in &mut self.pending {
            if !pending.sent
                && let Some(paths) = pending.job.ready()
            {
                pending.sent = true;
                self.updates.push_back(FileUpdate::Ready {
                    id: pending.id,
                    paths,
                });
            }
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .filter(|p| !p.sent)
            .map(|p| p.job.deadline())
            .min()
    }

    pub(super) fn close(&mut self) {
        self.updates.clear();
        for pending in self.pending.drain(..) {
            self.updates.push_back(FileUpdate::Ready {
                id: pending.id,
                paths: Err(()),
            });
            pending.offer.destroy();
            // Job's destructor cancels and joins the owned pipe reader.
        }
    }
}

impl Bridge {
    pub(crate) fn file_updates(&mut self) -> Vec<FileUpdate> {
        self.state.drag.files.updates.drain(..).collect()
    }

    pub(crate) fn finish_files(&mut self, id: u64, accepted: bool) {
        let files = &mut self.state.drag.files;
        if let Some(index) = files.pending.iter().position(|p| p.id == id && p.sent) {
            let pending = files.pending.swap_remove(index);
            if accepted {
                pending.offer.finish();
            }
            pending.offer.destroy();
            let _ = self.connection.flush();
        }
    }
}
