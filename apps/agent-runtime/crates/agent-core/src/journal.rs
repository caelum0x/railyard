//! Crash-durable record of the captures a [`CaptureReconciler`] owes.
//!
//! [`CaptureReconciler`]: crate::reconcile::CaptureReconciler
//!
//! Every change of a pending capture (queued, attempted, settled,
//! dead-lettered) is appended to the journal before the reconciler acts on
//! it, so after a crash the reconciler reopens with every capture still owed
//! (and every dead letter still visible) instead of losing that revenue.
//!
//! [`FileCaptureJournal`] is an append-only JSON-lines file, fsynced per
//! append and compacted on open (rewritten to the live state through a
//! temporary file and an atomic rename). One process owns a journal file.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aitp_client::{AmountMicros, HoldRef};
use serde::{Deserialize, Serialize};

/// One journal entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JournalEntry {
    /// A capture the provider served but Mint has not confirmed. `context` is
    /// the gateway's hold context (see `PaymentGateway::hold_context`).
    Enqueued {
        hold: String,
        amount: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<serde_json::Value>,
    },
    /// A failed attempt; `attempts` is the running count.
    Attempted { hold: String, attempts: usize },
    /// Mint confirmed the capture.
    Settled { hold: String },
    /// Abandoned after exhausting retries; money owed, settled out of band.
    DeadLettered {
        hold: String,
        attempts: usize,
        error: String,
    },
}

/// A capture still owed, as recovered from the journal.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledCapture {
    pub hold: HoldRef,
    pub amount: AmountMicros,
    pub attempts: usize,
    pub context: Option<serde_json::Value>,
}

/// A dead-lettered capture, as recovered from the journal.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledDeadLetter {
    pub hold: HoldRef,
    pub amount: AmountMicros,
    pub attempts: usize,
    pub error: String,
}

/// What a journal holds after replay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalState {
    pub pending: Vec<JournaledCapture>,
    pub dead_letters: Vec<JournaledDeadLetter>,
}

/// A journal failure.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("capture journal i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("capture journal line {line} is not a journal entry: {reason}")]
    Corrupt { line: usize, reason: String },
}

/// Where the reconciler records its owed captures.
pub trait CaptureJournal: Send + Sync {
    /// Durably record one entry (returns once it would survive a crash).
    fn append(&self, entry: &JournalEntry) -> Result<(), JournalError>;
    /// The state the entries describe.
    fn load(&self) -> Result<JournalState, JournalError>;
}

/// Replay entries into the live state (later entries win; entries for an
/// unknown hold are ignored).
pub fn replay(entries: impl IntoIterator<Item = JournalEntry>) -> JournalState {
    let mut pending: BTreeMap<String, JournaledCapture> = BTreeMap::new();
    let mut amounts: BTreeMap<String, i64> = BTreeMap::new();
    let mut dead: Vec<JournaledDeadLetter> = Vec::new();
    for entry in entries {
        match entry {
            JournalEntry::Enqueued {
                hold,
                amount,
                context,
            } => {
                amounts.insert(hold.clone(), amount);
                pending.entry(hold.clone()).or_insert(JournaledCapture {
                    hold: HoldRef(hold),
                    amount: AmountMicros(amount),
                    attempts: 0,
                    context,
                });
            }
            JournalEntry::Attempted { hold, attempts } => {
                if let Some(p) = pending.get_mut(&hold) {
                    p.attempts = attempts;
                }
            }
            JournalEntry::Settled { hold } => {
                pending.remove(&hold);
            }
            JournalEntry::DeadLettered {
                hold,
                attempts,
                error,
            } => {
                pending.remove(&hold);
                let amount = amounts.get(&hold).copied().unwrap_or_default();
                dead.retain(|d| d.hold.0 != hold);
                dead.push(JournaledDeadLetter {
                    hold: HoldRef(hold),
                    amount: AmountMicros(amount),
                    attempts,
                    error,
                });
            }
        }
    }
    JournalState {
        pending: pending.into_values().collect(),
        dead_letters: dead,
    }
}

/// The entries that rebuild `state` (what compaction writes).
fn entries_of(state: &JournalState) -> Vec<JournalEntry> {
    let mut out = Vec::new();
    for p in &state.pending {
        out.push(JournalEntry::Enqueued {
            hold: p.hold.0.clone(),
            amount: p.amount.0,
            context: p.context.clone(),
        });
        if p.attempts > 0 {
            out.push(JournalEntry::Attempted {
                hold: p.hold.0.clone(),
                attempts: p.attempts,
            });
        }
    }
    for d in &state.dead_letters {
        out.push(JournalEntry::Enqueued {
            hold: d.hold.0.clone(),
            amount: d.amount.0,
            context: None,
        });
        out.push(JournalEntry::DeadLettered {
            hold: d.hold.0.clone(),
            attempts: d.attempts,
            error: d.error.clone(),
        });
    }
    out
}

/// An append-only JSON-lines journal file.
pub struct FileCaptureJournal {
    path: PathBuf,
    file: Mutex<File>,
}

impl FileCaptureJournal {
    /// Open (creating) the journal at `path`, compacting it to its live state.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, JournalError> {
        let path = path.into();
        let state = if path.exists() {
            replay(read_entries(&path)?)
        } else {
            JournalState::default()
        };
        let tmp = path.with_extension("compact");
        {
            let mut out = File::create(&tmp)?;
            for entry in entries_of(&state) {
                write_line(&mut out, &entry)?;
            }
            out.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            // Make the rename itself durable.
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn write_line(out: &mut File, entry: &JournalEntry) -> Result<(), JournalError> {
    let mut line = serde_json::to_vec(entry).map_err(|e| JournalError::Corrupt {
        line: 0,
        reason: e.to_string(),
    })?;
    line.push(b'\n');
    out.write_all(&line)?;
    Ok(())
}

fn read_entries(path: &Path) -> Result<Vec<JournalEntry>, JournalError> {
    let reader = BufReader::new(File::open(path)?);
    let mut out = Vec::new();
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;
    let last = lines.len();
    for (i, line) in lines.into_iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalEntry>(&line) {
            Ok(entry) => out.push(entry),
            // A torn final line (a crash mid-append) is dropped; anything
            // earlier being unreadable is corruption, never silently skipped.
            Err(_) if i + 1 == last => {}
            Err(e) => {
                return Err(JournalError::Corrupt {
                    line: i + 1,
                    reason: e.to_string(),
                })
            }
        }
    }
    Ok(out)
}

impl CaptureJournal for FileCaptureJournal {
    fn append(&self, entry: &JournalEntry) -> Result<(), JournalError> {
        let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
        write_line(&mut file, entry)?;
        file.sync_data()?;
        Ok(())
    }

    fn load(&self) -> Result<JournalState, JournalError> {
        Ok(replay(read_entries(&self.path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agent-capture-{name}-{nanos}.jsonl"))
    }

    #[test]
    fn replay_keeps_owed_captures_and_dead_letters() {
        let state = replay([
            JournalEntry::Enqueued {
                hold: "h1".into(),
                amount: 10,
                context: None,
            },
            JournalEntry::Enqueued {
                hold: "h2".into(),
                amount: 20,
                context: Some(serde_json::json!({"payee":"p"})),
            },
            JournalEntry::Attempted {
                hold: "h2".into(),
                attempts: 2,
            },
            JournalEntry::Enqueued {
                hold: "h3".into(),
                amount: 30,
                context: None,
            },
            JournalEntry::Settled { hold: "h1".into() },
            JournalEntry::DeadLettered {
                hold: "h3".into(),
                attempts: 8,
                error: "down".into(),
            },
        ]);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].hold, HoldRef("h2".into()));
        assert_eq!(
            (state.pending[0].amount, state.pending[0].attempts),
            (AmountMicros(20), 2)
        );
        assert_eq!(
            state.pending[0].context,
            Some(serde_json::json!({"payee":"p"}))
        );
        assert_eq!(state.dead_letters.len(), 1);
        assert_eq!(
            (state.dead_letters[0].amount, state.dead_letters[0].attempts),
            (AmountMicros(30), 8)
        );
    }

    #[test]
    fn the_file_survives_reopen_compacts_and_drops_only_a_torn_tail() {
        let path = tmp("file");
        {
            let j = FileCaptureJournal::open(&path).unwrap();
            j.append(&JournalEntry::Enqueued {
                hold: "h1".into(),
                amount: 10,
                context: None,
            })
            .unwrap();
            j.append(&JournalEntry::Enqueued {
                hold: "h2".into(),
                amount: 20,
                context: None,
            })
            .unwrap();
            j.append(&JournalEntry::Settled { hold: "h1".into() })
                .unwrap();
        }
        // A crash mid-append leaves a torn last line.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"op\":\"settl")
            .unwrap();
        let j = FileCaptureJournal::open(&path).unwrap();
        let state = j.load().unwrap();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].hold, HoldRef("h2".into()));
        // Compacted: only the live entry is left.
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);

        // Corruption before the tail is an error, not silent loss.
        std::fs::write(&path, "not json\n{\"op\":\"settled\",\"hold\":\"h2\"}\n").unwrap();
        assert!(matches!(
            FileCaptureJournal::open(&path),
            Err(JournalError::Corrupt { line: 1, .. })
        ));
        let _ = std::fs::remove_file(&path);
    }
}
