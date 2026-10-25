//! What the bridge reads back from `jobs.get` (Zeus 05 section 9.2), and the status it reports to `ogund`.
//!
//! The `JobView` Zeus returns has every URL redacted (mode `log`) and never carries session material, so nothing
//! here is secret. Only the members the bridge needs are parsed; unknown members are ignored (Zeus 00 section 4.1).

use std::collections::BTreeMap;
use std::path::PathBuf;

use ogun_run::NodeState;
use serde::{Deserialize, Serialize};
use zeus_types::{ErrorEnvelope, JobState, Timestamp};

/// The parts of a Zeus `JobSpecView` the bridge uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SpecView {
    /// The stable URL, redacted.
    #[serde(default)]
    pub url_redacted: Option<String>,
    /// The referrer, redacted.
    #[serde(default)]
    pub referrer_redacted: Option<String>,
    /// The file name hint.
    #[serde(default)]
    pub filename: Option<String>,
    /// The session origin.
    #[serde(default)]
    pub session_origin: Option<String>,
    /// Expected SHA-256.
    #[serde(default)]
    pub expected_sha256: Option<String>,
    /// Expected size.
    #[serde(default)]
    pub expected_size: Option<u64>,
    /// Labels (untrusted).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Who asked.
    #[serde(default)]
    pub source: Option<SourceView>,
}

/// A job's source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SourceView {
    /// `agent` for jobs a service enqueued.
    #[serde(default)]
    pub kind: Option<String>,
}

/// The probe parts the provenance keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ProbeView {
    /// HTTP status.
    #[serde(default)]
    pub status: Option<u16>,
    /// Final URL, redacted.
    #[serde(default)]
    pub final_url_redacted: Option<String>,
    /// Declared content type.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Validators.
    #[serde(default)]
    pub validators: Option<ValidatorsView>,
}

/// HTTP validators.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ValidatorsView {
    /// `ETag`.
    #[serde(default)]
    pub etag: Option<String>,
    /// `Last-Modified`.
    #[serde(default)]
    pub last_modified: Option<String>,
    /// `Repr-Digest`.
    #[serde(default)]
    pub repr_digest: Option<String>,
}

/// Progress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ProgressView {
    /// Committed bytes.
    #[serde(default)]
    pub bytes_done: u64,
    /// Total, when known.
    #[serde(default)]
    pub bytes_total: Option<u64>,
}

/// A completed job's result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResultView {
    /// Final path, inside the destination root.
    pub path: String,
    /// Size.
    pub size_bytes: u64,
    /// SHA-256.
    pub sha256: String,
    /// BLAKE3.
    pub blake3: String,
    /// Detected media type.
    #[serde(default)]
    pub detected_mime: Option<String>,
    /// Completion.
    pub completed_at: Timestamp,
}

/// The parts of a Zeus `JobView` the bridge uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JobView {
    /// `job_...`.
    pub id: String,
    /// State.
    pub state: JobState,
    /// Quarantine flag.
    #[serde(default)]
    pub quarantined: bool,
    /// Quarantine reasons, as Zeus names them.
    #[serde(default)]
    pub quarantine_reasons: Vec<String>,
    /// Spec, redacted.
    #[serde(default)]
    pub spec: SpecView,
    /// Probe.
    #[serde(default)]
    pub probe: Option<ProbeView>,
    /// Progress.
    #[serde(default)]
    pub progress: Option<ProgressView>,
    /// Error, for failed jobs.
    #[serde(default)]
    pub error: Option<ErrorEnvelope>,
    /// Result, for completed jobs.
    #[serde(default)]
    pub result: Option<ResultView>,
    /// The pending Zeus approval.
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// One file a completed Zeus job produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeusFile {
    /// Absolute path inside Ogun's registered root.
    pub path: PathBuf,
    /// Size.
    pub size_bytes: u64,
    /// SHA-256.
    pub sha256: String,
    /// BLAKE3: the identity in Ogun's store.
    pub blake3: String,
    /// Detected media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Where a Zeus download came from, as Zeus reported it. Every URL is already redacted by Zeus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeusProvenance {
    /// The Zeus job.
    pub job_id: String,
    /// The requested URL, redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_redacted: Option<String>,
    /// The final URL, redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url_redacted: Option<String>,
    /// The referrer, redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_redacted: Option<String>,
    /// The origin whose browser session Zeus used (a grant a person approved in Zeus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<String>,
    /// When Zeus completed the download.
    pub completed_at: Timestamp,
    /// HTTP status of the data response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// `ETag`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// `Last-Modified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// Declared content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_media_type: Option<String>,
    /// Zeus quarantined the file; the flag survives import.
    pub quarantined: bool,
    /// Why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantine_reasons: Vec<String>,
}

/// A completed Zeus job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedJob {
    /// Its files (one per job in Zeus 05 v1).
    pub files: Vec<ZeusFile>,
    /// Its provenance.
    pub provenance: ZeusProvenance,
}

/// A Zeus job as `ogund` sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobStatus {
    /// Queued, probing, downloading, paused, verifying, finalizing or waiting to retry.
    Running {
        /// The Zeus state.
        state: JobState,
        /// Committed bytes.
        bytes_done: u64,
        /// Total, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
    },
    /// Waiting for a Zeus approval (a session grant). Only a person on a Zeus trust surface decides it; Ogun
    /// shows the id and links to Zeus.
    WaitingForApproval {
        /// The Zeus approval (`ap_...`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_id: Option<String>,
    },
    /// Waiting for session material, a fresh URL or the in-browser proxy (Zeus `awaiting_session`).
    WaitingForSession,
    /// Done: files and provenance.
    Succeeded(CompletedJob),
    /// Failed in Zeus.
    Failed {
        /// Zeus's error envelope.
        error: ErrorEnvelope,
    },
    /// Cancelled in Zeus.
    Cancelled,
}

impl JobStatus {
    /// The Zeus job state this status came from, for the O-01 section 5.4 mapping.
    #[must_use]
    pub fn node_state(&self) -> NodeState {
        match self {
            Self::Running { state, .. } => ogun_run::from_job_state(*state),
            Self::WaitingForApproval { .. } => ogun_run::from_job_state(JobState::AwaitingApproval),
            Self::WaitingForSession => ogun_run::from_job_state(JobState::AwaitingSession),
            Self::Succeeded(_) => ogun_run::from_job_state(JobState::Completed),
            Self::Failed { .. } => ogun_run::from_job_state(JobState::Failed),
            Self::Cancelled => ogun_run::from_job_state(JobState::Canceled),
        }
    }

    /// Whether Zeus will not change this job any more without a person (completed, failed or cancelled).
    #[must_use]
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Succeeded(_) | Self::Failed { .. } | Self::Cancelled
        )
    }
}

impl JobView {
    /// The provenance Zeus reported, whatever the state.
    #[must_use]
    pub fn provenance(&self, completed_at: Timestamp) -> ZeusProvenance {
        let probe = self.probe.clone().unwrap_or_default();
        let validators = probe.validators.clone().unwrap_or_default();
        ZeusProvenance {
            job_id: self.id.clone(),
            url_redacted: self.spec.url_redacted.clone(),
            final_url_redacted: probe.final_url_redacted.clone(),
            referrer_redacted: self.spec.referrer_redacted.clone(),
            session_origin: self.spec.session_origin.clone(),
            completed_at,
            http_status: probe.status,
            etag: validators.etag,
            last_modified: validators.last_modified,
            declared_media_type: probe
                .content_type
                .as_deref()
                .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned()),
            quarantined: self.quarantined,
            quarantine_reasons: self.quarantine_reasons.clone(),
        }
    }

    /// The status `ogund` acts on.
    #[must_use]
    pub fn status(&self) -> JobStatus {
        let progress = self.progress.clone().unwrap_or_default();
        match self.state {
            JobState::AwaitingApproval => JobStatus::WaitingForApproval {
                approval_id: self.approval_id.clone(),
            },
            JobState::AwaitingSession => JobStatus::WaitingForSession,
            JobState::Completed => match &self.result {
                Some(r) => JobStatus::Succeeded(CompletedJob {
                    files: vec![ZeusFile {
                        path: PathBuf::from(&r.path),
                        size_bytes: r.size_bytes,
                        sha256: r.sha256.clone(),
                        blake3: r.blake3.clone(),
                        media_type: r.detected_mime.clone(),
                    }],
                    provenance: self.provenance(r.completed_at),
                }),
                None => JobStatus::Failed {
                    error: ErrorEnvelope::new(
                        zeus_types::ErrorCode::from_static("internal.bug"),
                        zeus_types::ErrorClass::Internal,
                        false,
                        "completed job without a result",
                    ),
                },
            },
            JobState::Failed => JobStatus::Failed {
                error: self.error.clone().unwrap_or_else(|| {
                    ErrorEnvelope::new(
                        zeus_types::ErrorCode::from_static("internal.bug"),
                        zeus_types::ErrorClass::Internal,
                        false,
                        "failed job without an error",
                    )
                }),
            },
            JobState::Canceled => JobStatus::Cancelled,
            state => JobStatus::Running {
                state,
                bytes_done: progress.bytes_done,
                bytes_total: progress.bytes_total,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_job_view_parses_and_maps() {
        // From Zeus 05 section 9.2 (`jobs.get`), trimmed.
        let text = r#"{"id":"job_01M45NHJ2AHWMXT8JVJ7BEEJX7","state":"downloading","quarantined":false,"spec":{"url_redacted":"https://portal.example.org/download/8841","filename":"dataset-v3.tar.zst","session_origin":"https://portal.example.org","source":{"kind":"browser_handoff"}},"created_at":"2026-10-05T09:15:30.250Z","progress":{"bytes_done":611057664,"bytes_total":1288490188,"rate_ewma":15728640,"eta_s":43,"segments_active":4,"last_error_class":null,"updated_at":"2026-10-05T09:16:10.000Z"},"error":null,"result":null,"approval_id":"ap_01M45NHJ3XS0QXG9T42HHN2YR8"}"#;
        let view: JobView = serde_json::from_str(text).unwrap();
        let st = view.status();
        assert_eq!(st.node_state(), NodeState::Running);
        assert!(matches!(
            st,
            JobStatus::Running {
                bytes_done: 611_057_664,
                ..
            }
        ));
        let waiting = JobView {
            state: JobState::AwaitingApproval,
            ..view
        };
        assert_eq!(waiting.status().node_state(), NodeState::WaitingForUser);
    }
}
