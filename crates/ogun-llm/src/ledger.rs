//! The spend ledger, `<state_dir>/llm-ledger.jsonl` (O-03 section 8.9).
//!
//! Append-only JSON lines, one per model call, written by the daemon from the `usage` lines model operators
//! emit. Each entry carries the BLAKE3 digest of its own canonical form (JCS, without the `blake3` member) and
//! the digest of the entry before it, so any edit, removal or reordering breaks the chain. The daemon is the
//! only writer and holds an exclusive file lock while the ledger is open.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use ogun_types::{NodeInstanceId, RunId, Timestamp, WorkflowId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::LlmError;

/// File name of the ledger inside the state directory.
pub const LEDGER_FILE: &str = "llm-ledger.jsonl";

/// `prev_blake3` of the first entry.
pub const GENESIS_BLAKE3: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// What the daemon records for one model call: the operator's `usage` line plus where it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// The run.
    pub run_id: RunId,
    /// The workflow, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<WorkflowId>,
    /// The node instance that made the call.
    pub node_instance: NodeInstanceId,
    /// The operator reference, `name@major`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// Provider name from the ladder.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Input tokens as the provider reported them.
    pub input_tokens: u64,
    /// Output tokens (including reasoning tokens) as the provider reported them.
    pub output_tokens: u64,
    /// Cost in micro-US-dollars.
    pub cost_microusd: u64,
}

/// One ledger line. (serde cannot deny unknown members across `flatten`; the digest covers what is read.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Position, from 1, without gaps.
    pub seq: u64,
    /// When the daemon recorded the call.
    pub at: Timestamp,
    /// The call.
    #[serde(flatten)]
    pub usage: UsageRecord,
    /// `blake3` of the previous entry, or [`GENESIS_BLAKE3`].
    pub prev_blake3: String,
    /// BLAKE3 of the JCS form of this entry without this member.
    pub blake3: String,
}

impl LedgerEntry {
    /// The digest this entry must carry.
    pub fn expected_blake3(&self) -> Result<String, LlmError> {
        let mut v = serde_json::to_value(self)
            .map_err(|_| LlmError::Ledger("cannot serialise an entry".into()))?;
        if let Value::Object(map) = &mut v {
            map.remove("blake3");
        }
        let bytes = zeus_verify::canonical::to_jcs_bytes(&v)
            .map_err(|_| LlmError::Ledger("cannot canonicalise an entry".into()))?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }

    /// The UTC day, `YYYY-MM-DD`.
    #[must_use]
    pub fn day(&self) -> String {
        self.at.to_rfc3339()[..10].to_owned()
    }
}

/// Totals over a set of entries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Number of calls.
    pub calls: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Cost in micro-US-dollars.
    pub cost_microusd: u64,
}

impl Summary {
    fn add(&mut self, u: &UsageRecord) {
        self.calls = self.calls.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(u.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(u.output_tokens);
        self.cost_microusd = self.cost_microusd.saturating_add(u.cost_microusd);
    }
}

/// The result of verifying a ledger file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The entries, in order.
    pub entries: Vec<LedgerEntry>,
    /// Byte length of the verified prefix (everything but a torn final line).
    pub verified_len: u64,
    /// Whether a torn final line (a crash during a write) follows the verified prefix.
    pub torn_tail: bool,
}

impl Verified {
    /// The digest of the last entry, or [`GENESIS_BLAKE3`].
    #[must_use]
    pub fn head(&self) -> &str {
        self.entries
            .last()
            .map_or(GENESIS_BLAKE3, |e| e.blake3.as_str())
    }
}

/// Verifies ledger text: sequence, chain and digests. A final line without a newline that does not parse is
/// reported as a torn tail rather than an error; anything else that does not verify is an error naming the
/// line.
pub fn verify_text(text: &str) -> Result<Verified, LlmError> {
    let mut entries: Vec<LedgerEntry> = Vec::new();
    let mut offset = 0u64;
    let mut rest = text;
    let mut line_no = 0u64;
    while !rest.is_empty() {
        line_no += 1;
        let (line, complete) = match rest.find('\n') {
            Some(i) => (&rest[..i], true),
            None => (rest, false),
        };
        let consumed = line.len() + usize::from(complete);
        let parsed: Result<LedgerEntry, _> = serde_json::from_str(line);
        let entry = match parsed {
            Ok(e) => e,
            Err(_) if !complete => {
                return Ok(Verified {
                    entries,
                    verified_len: offset,
                    torn_tail: true,
                });
            }
            Err(_) => {
                return Err(LlmError::Ledger(format!(
                    "line {line_no} is not a ledger entry"
                )));
            }
        };
        let prev = entries.last().map_or(GENESIS_BLAKE3, |e| e.blake3.as_str());
        let want_seq = entries.last().map_or(1, |e| e.seq + 1);
        if entry.seq != want_seq {
            return Err(LlmError::Ledger(format!(
                "line {line_no}: sequence {} where {want_seq} was expected",
                entry.seq
            )));
        }
        if entry.prev_blake3 != prev {
            return Err(LlmError::Ledger(format!(
                "line {line_no}: the chain is broken"
            )));
        }
        if entry.blake3 != entry.expected_blake3()? {
            return Err(LlmError::Ledger(format!(
                "line {line_no}: the digest does not match the entry"
            )));
        }
        if !complete {
            // A complete entry whose newline was lost: keep it; the writer restores the newline.
            entries.push(entry);
            return Ok(Verified {
                entries,
                verified_len: offset + consumed as u64,
                torn_tail: true,
            });
        }
        entries.push(entry);
        offset += consumed as u64;
        rest = &rest[consumed..];
    }
    Ok(Verified {
        entries,
        verified_len: offset,
        torn_tail: false,
    })
}

/// Verifies a ledger file without opening it for writing. A missing file is an empty ledger.
pub fn verify_path(path: &Path) -> Result<Verified, LlmError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| LlmError::Ledger("the ledger is not UTF-8".into()))?;
            verify_text(text)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Verified {
            entries: Vec::new(),
            verified_len: 0,
            torn_tail: false,
        }),
        Err(e) => Err(LlmError::Ledger(format!("cannot read: {}", e.kind()))),
    }
}

/// The open ledger: the append handle, the chain head and running summaries.
#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    file: File,
    head_seq: u64,
    head_blake3: String,
    total: Summary,
    by_run: HashMap<RunId, Summary>,
    by_workflow: HashMap<WorkflowId, Summary>,
    by_day: BTreeMap<String, Summary>,
}

impl Ledger {
    /// Opens `<state_dir>/llm-ledger.jsonl`, creating it when missing.
    pub fn open(state_dir: &Path) -> Result<Self, LlmError> {
        Self::open_path(&state_dir.join(LEDGER_FILE))
    }

    /// Opens a ledger file: takes the exclusive lock, verifies the whole chain, cuts a torn final line and
    /// rebuilds the summaries. A chain that does not verify is an error; the file is left untouched for
    /// inspection.
    pub fn open_path(path: &Path) -> Result<Self, LlmError> {
        let io = |what: &str, e: std::io::Error| LlmError::Ledger(format!("{what}: {}", e.kind()));
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| io("cannot open", e))?;
        file.try_lock()
            .map_err(|_| LlmError::Ledger("another process holds the ledger".into()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| io("cannot read", e))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| LlmError::Ledger("the ledger is not UTF-8".into()))?;
        let verified = verify_text(text)?;
        if verified.torn_tail {
            tracing::warn!("llm ledger: repairing a torn final line");
            file.set_len(verified.verified_len)
                .map_err(|e| io("cannot truncate", e))?;
            file.seek(SeekFrom::End(0))
                .map_err(|e| io("cannot seek", e))?;
            if verified.verified_len > 0 && !text[..verified.verified_len as usize].ends_with('\n')
            {
                file.write_all(b"\n")
                    .and_then(|()| file.sync_data())
                    .map_err(|e| io("cannot write", e))?;
            }
        }
        let mut ledger = Self {
            path: path.to_owned(),
            file,
            head_seq: 0,
            head_blake3: GENESIS_BLAKE3.to_owned(),
            total: Summary::default(),
            by_run: HashMap::new(),
            by_workflow: HashMap::new(),
            by_day: BTreeMap::new(),
        };
        for e in &verified.entries {
            ledger.account(e);
        }
        Ok(ledger)
    }

    fn account(&mut self, e: &LedgerEntry) {
        self.head_seq = e.seq;
        self.head_blake3.clone_from(&e.blake3);
        self.total.add(&e.usage);
        self.by_run.entry(e.usage.run_id).or_default().add(&e.usage);
        if let Some(wf) = e.usage.workflow_id {
            self.by_workflow.entry(wf).or_default().add(&e.usage);
        }
        self.by_day.entry(e.day()).or_default().add(&e.usage);
    }

    /// Appends one call, recorded now. The line is on disk (`fsync`) when this returns.
    pub fn append(&mut self, usage: UsageRecord) -> Result<LedgerEntry, LlmError> {
        self.append_at(usage, Timestamp::now())
    }

    /// Appends one call with an explicit time (tests, imports).
    pub fn append_at(
        &mut self,
        usage: UsageRecord,
        at: Timestamp,
    ) -> Result<LedgerEntry, LlmError> {
        let mut entry = LedgerEntry {
            seq: self.head_seq + 1,
            at,
            usage,
            prev_blake3: self.head_blake3.clone(),
            blake3: String::new(),
        };
        entry.blake3 = entry.expected_blake3()?;
        let mut line = serde_json::to_string(&entry)
            .map_err(|_| LlmError::Ledger("cannot serialise an entry".into()))?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|e| LlmError::Ledger(format!("cannot append: {}", e.kind())))?;
        self.account(&entry);
        Ok(entry)
    }

    /// The ledger file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.head_seq
    }

    /// True when nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.head_seq == 0
    }

    /// The digest of the last entry.
    #[must_use]
    pub fn head(&self) -> &str {
        &self.head_blake3
    }

    /// Everything ever recorded.
    #[must_use]
    pub fn total(&self) -> Summary {
        self.total
    }

    /// Spend of one run.
    #[must_use]
    pub fn by_run(&self, run: &RunId) -> Summary {
        self.by_run.get(run).copied().unwrap_or_default()
    }

    /// Spend of one workflow, over all its runs.
    #[must_use]
    pub fn by_workflow(&self, wf: &WorkflowId) -> Summary {
        self.by_workflow.get(wf).copied().unwrap_or_default()
    }

    /// Spend on one UTC day (`YYYY-MM-DD`).
    #[must_use]
    pub fn by_day(&self, day: &str) -> Summary {
        self.by_day.get(day).copied().unwrap_or_default()
    }

    /// Spend per UTC day, oldest first.
    #[must_use]
    pub fn days(&self) -> &BTreeMap<String, Summary> {
        &self.by_day
    }

    /// Re-reads the entries of one run from disk (for a detailed listing).
    pub fn entries_for_run(&self, run: &RunId) -> Result<Vec<LedgerEntry>, LlmError> {
        Ok(verify_path(&self.path)?
            .entries
            .into_iter()
            .filter(|e| &e.usage.run_id == run)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogun_types::IdGenerator;

    fn usage(run: RunId, ni: NodeInstanceId, cost: u64) -> UsageRecord {
        UsageRecord {
            run_id: run,
            workflow_id: None,
            node_instance: ni,
            op: Some("llm.extract@1".into()),
            provider: "anthropic-sonnet".into(),
            model: "claude-sonnet-5-5".into(),
            input_tokens: 1200,
            output_tokens: 300,
            cost_microusd: cost,
        }
    }

    #[test]
    fn chain_appends_verifies_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let ids = IdGenerator::new();
        let run = ids.run_id();
        let ni = ids.node_instance_id();
        let at = Timestamp::from_unix_millis(1_792_800_000_000).unwrap();
        {
            let mut l = Ledger::open(dir.path()).unwrap();
            l.append_at(usage(run, ni, 10), at).unwrap();
            l.append_at(usage(run, ni, 5), at).unwrap();
            assert_eq!(l.by_run(&run).cost_microusd, 15);
        }
        let l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l.total().calls, 2);
        assert_eq!(l.days().len(), 1);
        let day = l.days().keys().next().unwrap().clone();
        assert_eq!(l.by_day(&day).cost_microusd, 15);
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let ids = IdGenerator::new();
        let (run, ni) = (ids.run_id(), ids.node_instance_id());
        {
            let mut l = Ledger::open(dir.path()).unwrap();
            l.append(usage(run, ni, 10)).unwrap();
            l.append(usage(run, ni, 20)).unwrap();
        }
        let path = dir.path().join(LEDGER_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replacen("\"cost_microusd\":10", "\"cost_microusd\":1", 1),
        )
        .unwrap();
        assert!(Ledger::open(dir.path()).is_err());
    }

    #[test]
    fn torn_tail_is_cut() {
        let dir = tempfile::tempdir().unwrap();
        let ids = IdGenerator::new();
        let (run, ni) = (ids.run_id(), ids.node_instance_id());
        {
            let mut l = Ledger::open(dir.path()).unwrap();
            l.append(usage(run, ni, 10)).unwrap();
        }
        let path = dir.path().join(LEDGER_FILE);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"seq\":2,\"at\":").unwrap();
        drop(f);
        let mut l = Ledger::open(dir.path()).unwrap();
        assert_eq!(l.len(), 1);
        l.append(usage(run, ni, 1)).unwrap();
        drop(l);
        assert_eq!(verify_path(&path).unwrap().entries.len(), 2);
    }
}
