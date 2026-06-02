use std::time::Duration;

use ra::memory_entry::{
    decide_generation, decide_use, generation_lifecycle, use_lifecycle, DreamAdoptionDecision,
    DreamJob, DreamScheduler, DreamSkipReason, DreamStatus, GenerationDecision, MemoryCandidate,
    MemoryEntry, MemoryEntryLifecycle, MemoryPolicy, PendingReason, ShouldDreamDecision,
    SuppressionReason, UseDecision, DREAM_INPUT_SESSION_CAP,
};

fn enabled_policy() -> MemoryPolicy {
    MemoryPolicy {
        memories_enabled: true,
        min_idle_before_generation: Duration::from_secs(300),
        min_session_duration: Duration::from_secs(120),
        min_rate_limit_remaining_percent: 20,
        ..MemoryPolicy::default()
    }
}

fn mature_candidate() -> MemoryCandidate {
    MemoryCandidate {
        session_duration: Duration::from_secs(600),
        idle_for: Duration::from_secs(300),
        is_active: false,
        has_external_context: false,
        rate_limit_remaining_percent: Some(80),
        redaction_applied: false,
    }
}

#[test]
fn eligible_candidate_generates_durable_entry() {
    let decision = decide_generation(&enabled_policy(), &mature_candidate());

    let GenerationDecision::Allowed { entry } = decision else {
        panic!("expected allowed generation");
    };

    assert!(entry.durable);
    assert!(!entry.redaction_applied);
    assert_eq!(
        generation_lifecycle(&GenerationDecision::Allowed {
            entry: entry.clone()
        }),
        MemoryEntryLifecycle::Generated
    );
    assert_eq!(
        decide_use(&enabled_policy(), &entry, false),
        UseDecision::Active
    );
}

#[test]
fn global_region_thread_and_external_controls_suppress_generation() {
    let cases = [
        (
            MemoryPolicy {
                memories_enabled: false,
                ..enabled_policy()
            },
            mature_candidate(),
            SuppressionReason::MemoriesDisabled,
        ),
        (
            MemoryPolicy {
                region_available: false,
                ..enabled_policy()
            },
            mature_candidate(),
            SuppressionReason::RegionUnavailable,
        ),
        (
            MemoryPolicy {
                generate_memories: false,
                ..enabled_policy()
            },
            mature_candidate(),
            SuppressionReason::ThreadGenerationDisabled,
        ),
        (
            MemoryPolicy {
                disable_on_external_context: true,
                ..enabled_policy()
            },
            MemoryCandidate {
                has_external_context: true,
                ..mature_candidate()
            },
            SuppressionReason::ExternalContext,
        ),
    ];

    for (policy, candidate, expected) in cases {
        assert_eq!(
            decide_generation(&policy, &candidate),
            GenerationDecision::Skipped { reason: expected }
        );
    }
}

#[test]
fn active_or_short_lived_sessions_are_skipped() {
    let policy = enabled_policy();

    assert_eq!(
        decide_generation(
            &policy,
            &MemoryCandidate {
                is_active: true,
                ..mature_candidate()
            },
        ),
        GenerationDecision::Skipped {
            reason: SuppressionReason::SessionActive
        }
    );

    assert_eq!(
        decide_generation(
            &policy,
            &MemoryCandidate {
                session_duration: Duration::from_secs(30),
                ..mature_candidate()
            },
        ),
        GenerationDecision::Skipped {
            reason: SuppressionReason::SessionTooShort
        }
    );
}

#[test]
fn eligible_candidate_waits_for_idle_delay() {
    let decision = decide_generation(
        &enabled_policy(),
        &MemoryCandidate {
            idle_for: Duration::from_secs(120),
            ..mature_candidate()
        },
    );

    assert_eq!(
        decision,
        GenerationDecision::Pending {
            reason: PendingReason::WaitingForIdle,
            remaining_idle: Duration::from_secs(180),
        }
    );
    assert_eq!(
        generation_lifecycle(&decision),
        MemoryEntryLifecycle::PendingBackgroundGeneration
    );
}

#[test]
fn low_rate_limit_skips_background_generation_pass() {
    assert_eq!(
        decide_generation(
            &enabled_policy(),
            &MemoryCandidate {
                rate_limit_remaining_percent: Some(19),
                ..mature_candidate()
            },
        ),
        GenerationDecision::Skipped {
            reason: SuppressionReason::RateLimitTooLow
        }
    );
}

#[test]
fn use_gating_is_separate_from_generation_gating() {
    let policy = MemoryPolicy {
        generate_memories: false,
        use_memories: true,
        ..enabled_policy()
    };
    let entry = MemoryEntry::generated(false);

    assert_eq!(
        decide_generation(&policy, &mature_candidate()),
        GenerationDecision::Skipped {
            reason: SuppressionReason::ThreadGenerationDisabled
        }
    );
    assert_eq!(decide_use(&policy, &entry, false), UseDecision::Active);

    let no_use_policy = MemoryPolicy {
        use_memories: false,
        ..enabled_policy()
    };
    let decision = decide_use(&no_use_policy, &entry, false);
    assert_eq!(
        decision,
        UseDecision::Suppressed {
            reason: SuppressionReason::ThreadUseDisabled
        }
    );
    assert_eq!(
        use_lifecycle(&decision),
        MemoryEntryLifecycle::Suppressed(SuppressionReason::ThreadUseDisabled)
    );
}

#[test]
fn non_durable_entry_cannot_be_used() {
    assert_eq!(
        decide_use(&enabled_policy(), &MemoryEntry::transient(false), false),
        UseDecision::Suppressed {
            reason: SuppressionReason::EntryNotDurable
        }
    );
}

#[test]
fn generated_entries_record_redaction_and_guidance() {
    let decision = decide_generation(
        &enabled_policy(),
        &MemoryCandidate {
            redaction_applied: true,
            ..mature_candidate()
        },
    );

    let GenerationDecision::Allowed { entry } = decision else {
        panic!("expected allowed generation");
    };

    assert!(entry.redaction_applied);
    let guidance = entry.guidance();
    assert!(guidance.generated_local_state);
    assert!(guidance.inspectable_for_troubleshooting);
    assert_eq!(
        guidance.primary_control_surface,
        "settings and thread-level memory controls"
    );
    assert_eq!(
        guidance.authoritative_team_guidance,
        "AGENTS.md or checked-in documentation"
    );
}

#[test]
fn dream_scheduler_reports_each_skip_branch() {
    let scheduler = DreamScheduler::new(MemoryPolicy {
        memories_enabled: false,
        ..enabled_policy()
    });
    assert_eq!(
        scheduler.should_dream(10, Some(80)),
        ShouldDreamDecision::Skip(DreamSkipReason::MemoriesDisabled)
    );

    let scheduler = DreamScheduler::new(MemoryPolicy {
        region_available: false,
        ..enabled_policy()
    });
    assert_eq!(
        scheduler.should_dream(10, Some(80)),
        ShouldDreamDecision::Skip(DreamSkipReason::RegionUnavailable)
    );

    let scheduler = DreamScheduler::new(enabled_policy());
    assert_eq!(
        scheduler.should_dream(10, Some(19)),
        ShouldDreamDecision::Skip(DreamSkipReason::RateLimitTooLow)
    );
    assert_eq!(
        scheduler.should_dream(9, Some(80)),
        ShouldDreamDecision::Skip(DreamSkipReason::NotEnoughSessions)
    );
    assert_eq!(
        scheduler.should_dream(10, Some(80)),
        ShouldDreamDecision::Dream
    );
}

#[test]
fn dream_input_selection_filters_active_short_and_caps_inputs() {
    let scheduler = DreamScheduler::new(enabled_policy());
    let mut candidates = Vec::new();
    candidates.push(MemoryCandidate {
        is_active: true,
        ..mature_candidate()
    });
    candidates.push(MemoryCandidate {
        session_duration: Duration::from_secs(119),
        ..mature_candidate()
    });
    for _ in 0..(DREAM_INPUT_SESSION_CAP + 5) {
        candidates.push(mature_candidate());
    }

    let selected = scheduler.select_dream_inputs(&candidates);

    assert_eq!(selected.len(), DREAM_INPUT_SESSION_CAP);
    assert!(selected.iter().all(|candidate| !candidate.is_active));
    assert!(selected
        .iter()
        .all(|candidate| candidate.session_duration >= scheduler.policy().min_session_duration));
}

#[test]
fn completed_dream_output_reuses_memory_use_gate() {
    let mut job = DreamJob::new("dream-1", "input-store");
    job.update(DreamStatus::Completed, Some("output-store"));

    assert_eq!(
        job.adopt_output(&enabled_policy(), false),
        DreamAdoptionDecision::Adopted {
            output_store_id: "output-store".into()
        }
    );
    assert_eq!(job.input_store_id, "input-store");

    let no_use_policy = MemoryPolicy {
        use_memories: false,
        ..enabled_policy()
    };
    assert_eq!(
        job.adopt_output(&no_use_policy, false),
        DreamAdoptionDecision::Suppressed {
            reason: SuppressionReason::ThreadUseDisabled
        }
    );
}

#[test]
fn non_completed_or_missing_dream_output_is_not_adopted() {
    let policy = enabled_policy();
    let mut running = DreamJob::new("dream-1", "input-store");
    running.update(DreamStatus::Running, Some("partial-output"));
    assert_eq!(
        running.adopt_output(&policy, false),
        DreamAdoptionDecision::Suppressed {
            reason: SuppressionReason::EntryNotDurable
        }
    );

    let mut completed_without_output = DreamJob::new("dream-2", "input-store");
    completed_without_output.update(DreamStatus::Completed, None::<String>);
    assert_eq!(
        completed_without_output.adopt_output(&policy, false),
        DreamAdoptionDecision::Suppressed {
            reason: SuppressionReason::EntryNotDurable
        }
    );
}
