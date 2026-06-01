//! Policy model for Codex-style `memoryEntry` lifecycle decisions.
//!
//! This module is intentionally pure: it does not extract memories, spawn
//! background jobs, or read/write generated memory files. It captures the
//! lifecycle contract future integrations should share.

use std::time::Duration;

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
