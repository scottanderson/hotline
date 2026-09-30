//! Live transfer progress, for [`myTran_GetClientInfoText`] (303).
//!
//! [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::state::{PendingTransfer, TransferDirection, TransferKind, TransferRef};

/// Which section of the client info text a transfer belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferSection {
    /// A [`myTran_DownloadFile`] (202) download.
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    FileDownload,
    /// A [`myTran_DownloadFldr`] (210) download.
    ///
    /// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
    FolderDownload,
    /// A [`myTran_UploadFile`] (203) upload.
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    FileUpload,
    /// A [`myTran_UploadFldr`] (213) upload.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    FolderUpload,
}

impl TransferSection {
    /// `File` or `Folder`, as a waiting line shows it.
    const fn noun(self) -> &'static str {
        match self {
            Self::FileDownload | Self::FileUpload => "File",
            Self::FolderDownload | Self::FolderUpload => "Folder",
        }
    }
}

/// One transfer as the client info text shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    /// The section it is listed under.
    pub section: TransferSection,
    /// The file or folder name.
    pub name: Box<str>,
    /// The transfer size in bytes.
    pub size: u64,
    /// Bytes moved so far, `0` until the data connection arrives.
    pub done: u64,
}

impl TransferProgress {
    /// `None` for a banner.
    pub(crate) fn from_pending(transfer: &PendingTransfer) -> Option<Self> {
        let (section, name) = match (&transfer.kind, transfer.direction) {
            (TransferKind::File(file), TransferDirection::Get) => {
                (TransferSection::FileDownload, &file.target.name)
            }
            (TransferKind::Folder(folder), TransferDirection::Get) => {
                (TransferSection::FolderDownload, &folder.target.name)
            }
            (TransferKind::File(file), TransferDirection::Put) => {
                (TransferSection::FileUpload, &file.target.name)
            }
            (TransferKind::Folder(folder), TransferDirection::Put) => {
                (TransferSection::FolderUpload, &folder.target.name)
            }
            (TransferKind::Banner, _) => return None,
        };
        Some(Self {
            section,
            name: name.clone(),
            size: transfer.size,
            done: 0,
        })
    }

    /// openline's `%#-21.21s %#-4.4s %#s` transfer line.
    pub(crate) fn line(&self) -> String {
        let name = name_column(&self.name);
        let percent = format!("{}%", self.percent());
        format!("{name} {percent:<4} {}\r", size_text(self.size))
    }

    /// openline's `%#-21.21s (File)` or `(Folder)` waiting line.
    pub(crate) fn waiting_line(&self) -> String {
        format!("{} ({})\r", name_column(&self.name), self.section.noun())
    }

    /// openline's `done / (size / 100)`, at most 100, or 0 under 100 bytes.
    fn percent(&self) -> u64 {
        match self.size / 100 {
            0 => 0,
            step => (self.done / step).min(100),
        }
    }
}

/// `name` cut or padded to 21 characters.
fn name_column(name: &str) -> String {
    let name: String = name.chars().take(21).collect();
    format!("{name:<21}")
}

/// openline's `UText::SizeToText` with `kDontShowBytes`.
fn size_text(size: u64) -> String {
    const K: u64 = 1 << 10;
    const M: u64 = 1 << 20;
    match size {
        0 => "0K".to_string(),
        1..K => "<1K".to_string(),
        K..M => format!("{}K", size / K),
        _ => match (size % M) / 104_858 {
            0 => format!("{}M", size / M),
            tenth => format!("{}.{tenth}M", size / M),
        },
    }
}

/// One user's transfers, oldest first.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TransferReport {
    /// Downloads with a slot and every upload.
    pub active: Vec<TransferProgress>,
    /// Downloads waiting for a slot.
    pub waiting: Vec<TransferProgress>,
}

struct Running {
    user_id: u16,
    progress: TransferProgress,
    done: Arc<AtomicU64>,
}

/// Transfers whose data connection is open.
#[derive(Default)]
pub(crate) struct RunningTransfers(Mutex<HashMap<TransferRef, Running>>);

impl RunningTransfers {
    /// Track `transfer` until the returned guard drops. `None` for a banner.
    pub(crate) fn start(
        self: &Arc<Self>,
        reference: TransferRef,
        transfer: &PendingTransfer,
    ) -> Option<ProgressGuard> {
        let progress = TransferProgress::from_pending(transfer)?;
        let done = Arc::new(AtomicU64::new(transfer.resume_offset));
        let running = Running {
            user_id: transfer.user_id,
            progress,
            done: done.clone(),
        };
        self.0.lock().unwrap().insert(reference, running);
        Some(ProgressGuard {
            running: self.clone(),
            reference,
            done,
        })
    }

    /// `user_id`'s running transfers, with their byte counts now.
    pub(crate) fn for_user(&self, user_id: u16) -> Vec<(TransferRef, TransferProgress)> {
        let running = self.0.lock().unwrap();
        running
            .iter()
            .filter(|(_, r)| r.user_id == user_id)
            .map(|(&reference, r)| {
                let done = r.done.load(Ordering::Relaxed);
                (
                    reference,
                    TransferProgress {
                        done,
                        ..r.progress.clone()
                    },
                )
            })
            .collect()
    }
}

/// Removes its transfer from [`RunningTransfers`] on drop.
pub(crate) struct ProgressGuard {
    running: Arc<RunningTransfers>,
    reference: TransferRef,
    done: Arc<AtomicU64>,
}

impl ProgressGuard {
    /// The byte counter to add each moved byte to.
    pub(crate) fn counter(&self) -> Arc<AtomicU64> {
        self.done.clone()
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.running.0.lock().unwrap().remove(&self.reference);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_text_follows_openline_size_to_text() {
        let cases = [
            (0, "0K"),
            (300, "<1K"),
            (2047, "1K"),
            (1 << 20, "1M"),
            ((1 << 20) + 104_858, "1.1M"),
            ((3 << 20) + (1 << 19), "3.4M"),
        ];
        for (size, text) in cases {
            assert_eq!(size_text(size), text, "{size}");
        }
    }

    #[test]
    fn percent_divides_by_a_hundredth_of_the_size() {
        let progress = |size, done| TransferProgress {
            section: TransferSection::FileDownload,
            name: "a".into(),
            size,
            done,
        };
        assert_eq!(progress(1000, 500).percent(), 50);
        assert_eq!(progress(150, 75).percent(), 75);
        assert_eq!(progress(1000, 5000).percent(), 100);
        assert_eq!(progress(50, 50).percent(), 0);
    }

    #[test]
    fn line_cuts_a_long_name_to_21_characters() {
        let progress = TransferProgress {
            section: TransferSection::FileDownload,
            name: "a name longer than twenty-one".into(),
            size: 2 << 20,
            done: 1 << 20,
        };
        assert_eq!(progress.line(), "a name longer than tw 50%  2M\r");
    }

    #[test]
    fn waiting_line_names_a_folder_download_a_folder() {
        let progress = TransferProgress {
            section: TransferSection::FolderDownload,
            name: "stuff".into(),
            size: 0,
            done: 0,
        };
        assert_eq!(progress.waiting_line(), "stuff                 (Folder)\r");
    }
}
