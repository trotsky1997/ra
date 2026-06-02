//! Policy model for Codex-style `memoryEntry` lifecycle decisions.
//!
//! This module is intentionally pure: it does not extract memories, spawn
//! background jobs, or read/write generated memory files. It captures the
//! lifecycle contract future integrations should share.

use std::time::Duration;

/// Claude Dreams accepts at most 100 session transcripts per dream job.
pub const DREAM_INPUT_SESSION_CAP: usize = 100;

/// Policy switches and thresholds that apply to memory generation and use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPolicy {
    pub memories_enabled: bool,
    pub region_available: bool,
    pub generate_memories: bool,
    pub use_memories: bool,
    pub disable_on_external_context: bool,
    pub min_idle_before_generation: Duration,
    pub min_session_duration: Duration,
    pub min_rate_limit_remaining_percent: u8,
    pub min_sessions_between_dreams: usize,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            memories_enabled: false,
            region_available: true,
            generate_memories: true,
            use_memories: true,
            disable_on_external_context: false,
            min_idle_before_generation: Duration::from_secs(10 * 60),
            min_session_duration: Duration::from_secs(60),
            min_rate_limit_remaining_percent: 0,
            min_sessions_between_dreams: 10,
        }
    }
}

/// Observations about one thread/session that may contribute a memory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCandidate {
    pub session_duration: Duration,
    pub idle_for: Duration,
    pub is_active: bool,
    pub has_external_context: bool,
    pub rate_limit_remaining_percent: Option<u8>,
    pub redaction_applied: bool,
}

impl MemoryCandidate {
    pub fn new(session_duration: Duration, idle_for: Duration) -> Self {
        Self {
            session_duration,
            idle_for,
            is_active: false,
            has_external_context: false,
            rate_limit_remaining_percent: None,
            redaction_applied: false,
        }
    }
}

/// Metadata for a generated local memory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    pub durable: bool,
    pub redaction_applied: bool,
}

impl MemoryEntry {
    pub fn generated(redaction_applied: bool) -> Self {
        Self {
            durable: true,
            redaction_applied,
        }
    }

    pub fn transient(redaction_applied: bool) -> Self {
        Self {
            durable: false,
            redaction_applied,
        }
    }

    pub fn guidance(&self) -> MemoryEntryGuidance {
        MemoryEntryGuidance {
            generated_local_state: true,
            inspectable_for_troubleshooting: true,
            primary_control_surface: "settings and thread-level memory controls",
            authoritative_team_guidance: "AGENTS.md or checked-in documentation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryEntryGuidance {
    pub generated_local_state: bool,
    pub inspectable_for_troubleshooting: bool,
    pub primary_control_surface: &'static str,
    pub authoritative_team_guidance: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerationDecision {
    Allowed {
        entry: MemoryEntry,
    },
    Pending {
        reason: PendingReason,
        remaining_idle: Duration,
    },
    Skipped {
        reason: SuppressionReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseDecision {
    Active,
    Suppressed { reason: SuppressionReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReason {
    WaitingForIdle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    MemoriesDisabled,
    RegionUnavailable,
    ThreadGenerationDisabled,
    ThreadUseDisabled,
    ExternalContext,
    SessionActive,
    SessionTooShort,
    RateLimitTooLow,
    EntryNotDurable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryEntryLifecycle {
    Candidate,
    PendingBackgroundGeneration,
    Skipped(SuppressionReason),
    Generated,
    Durable,
    ActiveForUse,
    Suppressed(SuppressionReason),
}

/// Agent-side policy helper for Claude-style Dreams.
///
/// Dreams synthesize many sessions and an input memory store into a new output
/// memory store. They are intentionally modeled above `MemoryEntryLifecycle`
/// because they are not a per-entry lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamScheduler {
    policy: MemoryPolicy,
}

impl DreamScheduler {
    pub fn new(policy: MemoryPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &MemoryPolicy {
        &self.policy
    }

    /// Decide whether the agent should start a dream now.
    ///
    /// The agent owns the scheduling loop; this method only evaluates the
    /// memory policy gates that make the decision auditable.
    pub fn should_dream(
        &self,
        sessions_since_last_dream: usize,
        rate_limit_remaining_percent: Option<u8>,
    ) -> ShouldDreamDecision {
        if !self.policy.memories_enabled {
            return ShouldDreamDecision::Skip(DreamSkipReason::MemoriesDisabled);
        }
        if !self.policy.region_available {
            return ShouldDreamDecision::Skip(DreamSkipReason::RegionUnavailable);
        }
        if rate_limit_remaining_percent
            .is_some_and(|remaining| remaining < self.policy.min_rate_limit_remaining_percent)
        {
            return ShouldDreamDecision::Skip(DreamSkipReason::RateLimitTooLow);
        }
        if sessions_since_last_dream < self.policy.min_sessions_between_dreams {
            return ShouldDreamDecision::Skip(DreamSkipReason::NotEnoughSessions);
        }
        ShouldDreamDecision::Dream
    }

    /// Select eligible past sessions for a dream input batch.
    ///
    /// Active and too-short sessions are excluded. Idle delay is intentionally
    /// ignored here because Dreams consume prior sessions rather than deciding
    /// whether a just-finished session may generate an entry.
    pub fn select_dream_inputs<'a>(
        &self,
        candidates: &'a [MemoryCandidate],
    ) -> Vec<&'a MemoryCandidate> {
        candidates
            .iter()
            .filter(|candidate| {
                !candidate.is_active
                    && candidate.session_duration >= self.policy.min_session_duration
            })
            .take(DREAM_INPUT_SESSION_CAP)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShouldDreamDecision {
    Dream,
    Skip(DreamSkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DreamSkipReason {
    MemoriesDisabled,
    RegionUnavailable,
    RateLimitTooLow,
    NotEnoughSessions,
}

/// Agent-owned dream job state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamJob {
    pub dream_id: String,
    pub input_store_id: String,
    pub output_store_id: Option<String>,
    pub status: DreamStatus,
}

impl DreamJob {
    pub fn new(dream_id: impl Into<String>, input_store_id: impl Into<String>) -> Self {
        Self {
            dream_id: dream_id.into(),
            input_store_id: input_store_id.into(),
            output_store_id: None,
            status: DreamStatus::Pending,
        }
    }

    pub fn update(&mut self, status: DreamStatus, output_store_id: Option<impl Into<String>>) {
        self.status = status;
        if let Some(output_store_id) = output_store_id {
            self.output_store_id = Some(output_store_id.into());
        }
    }

    /// Decide whether the completed dream output may be adopted for future use.
    ///
    /// Adoption reuses the same memory use gate as ordinary generated durable
    /// memories. Non-completed jobs or completed jobs without an output store do
    /// not become active.
    pub fn adopt_output(
        &self,
        policy: &MemoryPolicy,
        has_external_context: bool,
    ) -> DreamAdoptionDecision {
        if self.status != DreamStatus::Completed {
            return DreamAdoptionDecision::Suppressed {
                reason: SuppressionReason::EntryNotDurable,
            };
        }
        let Some(output_store_id) = &self.output_store_id else {
            return DreamAdoptionDecision::Suppressed {
                reason: SuppressionReason::EntryNotDurable,
            };
        };
        let output_entry = MemoryEntry::generated(false);
        match decide_use(policy, &output_entry, has_external_context) {
            UseDecision::Active => DreamAdoptionDecision::Adopted {
                output_store_id: output_store_id.clone(),
            },
            UseDecision::Suppressed { reason } => DreamAdoptionDecision::Suppressed { reason },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DreamStatus {
    Pending,
    Running,
    Completed,
    Failed { error_type: String },
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DreamAdoptionDecision {
    Adopted { output_store_id: String },
    Suppressed { reason: SuppressionReason },
}

/// Decide whether a thread/session may generate a memory entry.
pub fn decide_generation(policy: &MemoryPolicy, candidate: &MemoryCandidate) -> GenerationDecision {
    if let Some(reason) = shared_suppression(policy, candidate.has_external_context) {
        return GenerationDecision::Skipped { reason };
    }
    if !policy.generate_memories {
        return GenerationDecision::Skipped {
            reason: SuppressionReason::ThreadGenerationDisabled,
        };
    }
    if candidate.is_active {
        return GenerationDecision::Skipped {
            reason: SuppressionReason::SessionActive,
        };
    }
    if candidate.session_duration < policy.min_session_duration {
        return GenerationDecision::Skipped {
            reason: SuppressionReason::SessionTooShort,
        };
    }
    if candidate.idle_for < policy.min_idle_before_generation {
        return GenerationDecision::Pending {
            reason: PendingReason::WaitingForIdle,
            remaining_idle: policy.min_idle_before_generation - candidate.idle_for,
        };
    }
    if candidate
        .rate_limit_remaining_percent
        .is_some_and(|remaining| remaining < policy.min_rate_limit_remaining_percent)
    {
        return GenerationDecision::Skipped {
            reason: SuppressionReason::RateLimitTooLow,
        };
    }

    GenerationDecision::Allowed {
        entry: MemoryEntry::generated(candidate.redaction_applied),
    }
}

/// Decide whether an existing memory entry may be used in the current thread.
pub fn decide_use(
    policy: &MemoryPolicy,
    entry: &MemoryEntry,
    has_external_context: bool,
) -> UseDecision {
    if let Some(reason) = shared_suppression(policy, has_external_context) {
        return UseDecision::Suppressed { reason };
    }
    if !policy.use_memories {
        return UseDecision::Suppressed {
            reason: SuppressionReason::ThreadUseDisabled,
        };
    }
    if !entry.durable {
        return UseDecision::Suppressed {
            reason: SuppressionReason::EntryNotDurable,
        };
    }
    UseDecision::Active
}

pub fn generation_lifecycle(decision: &GenerationDecision) -> MemoryEntryLifecycle {
    match decision {
        GenerationDecision::Allowed { .. } => MemoryEntryLifecycle::Generated,
        GenerationDecision::Pending { .. } => MemoryEntryLifecycle::PendingBackgroundGeneration,
        GenerationDecision::Skipped { reason } => MemoryEntryLifecycle::Skipped(*reason),
    }
}

pub fn use_lifecycle(decision: &UseDecision) -> MemoryEntryLifecycle {
    match decision {
        UseDecision::Active => MemoryEntryLifecycle::ActiveForUse,
        UseDecision::Suppressed { reason } => MemoryEntryLifecycle::Suppressed(*reason),
    }
}

fn shared_suppression(
    policy: &MemoryPolicy,
    has_external_context: bool,
) -> Option<SuppressionReason> {
    if !policy.memories_enabled {
        return Some(SuppressionReason::MemoriesDisabled);
    }
    if !policy.region_available {
        return Some(SuppressionReason::RegionUnavailable);
    }
    if policy.disable_on_external_context && has_external_context {
        return Some(SuppressionReason::ExternalContext);
    }
    None
}
