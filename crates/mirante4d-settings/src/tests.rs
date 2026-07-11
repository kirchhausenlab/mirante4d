use std::{ffi::OsString, fs, io, path::Path, time::Duration};

use super::*;

fn receive_event(actor: &SettingsActor) -> SettingsEvent {
    actor
        .events
        .as_ref()
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .expect("settings actor should report within the deterministic test timeout")
}

fn assert_no_owned_temporaries(parent: &Path) {
    let leftovers = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".settings.json.tmp-"))
        .collect::<Vec<_>>();
    assert_eq!(leftovers, Vec::<String>::new());
}

#[test]
fn resource_policy_enforces_exact_inclusive_bounds() {
    assert!(ResourcePolicy::new(MIN_CPU_DATASET_BUDGET_BYTES, MIN_GPU_BUDGET_BYTES).is_ok());
    assert!(ResourcePolicy::new(MAX_CPU_DATASET_BUDGET_BYTES, MAX_GPU_BUDGET_BYTES).is_ok());

    assert_eq!(
        ResourcePolicy::new(MIN_CPU_DATASET_BUDGET_BYTES - 1, MIN_GPU_BUDGET_BYTES),
        Err(SettingsError::CpuDatasetBudgetOutOfBounds {
            actual: MIN_CPU_DATASET_BUDGET_BYTES - 1,
            minimum: MIN_CPU_DATASET_BUDGET_BYTES,
            maximum: MAX_CPU_DATASET_BUDGET_BYTES,
        })
    );
    assert_eq!(
        ResourcePolicy::new(MAX_CPU_DATASET_BUDGET_BYTES + 1, MIN_GPU_BUDGET_BYTES),
        Err(SettingsError::CpuDatasetBudgetOutOfBounds {
            actual: MAX_CPU_DATASET_BUDGET_BYTES + 1,
            minimum: MIN_CPU_DATASET_BUDGET_BYTES,
            maximum: MAX_CPU_DATASET_BUDGET_BYTES,
        })
    );
    assert!(matches!(
        ResourcePolicy::new(MIN_CPU_DATASET_BUDGET_BYTES, MIN_GPU_BUDGET_BYTES - 1),
        Err(SettingsError::GpuBudgetOutOfBounds { .. })
    ));
    assert!(matches!(
        ResourcePolicy::new(MIN_CPU_DATASET_BUDGET_BYTES, MAX_GPU_BUDGET_BYTES + 1),
        Err(SettingsError::GpuBudgetOutOfBounds { .. })
    ));
}

#[test]
fn recommended_policy_uses_the_approved_formulas() {
    assert_eq!(
        ResourcePolicy::recommended(None, None).unwrap(),
        ResourcePolicy::new(4 * GIB, GIB).unwrap()
    );
    assert_eq!(
        ResourcePolicy::recommended(Some(GIB), Some(4 * GIB)).unwrap(),
        ResourcePolicy::new(2 * GIB, 2 * GIB).unwrap()
    );
    assert_eq!(
        ResourcePolicy::recommended(Some(20 * GIB), Some(8 * GIB)).unwrap(),
        ResourcePolicy::new(8 * GIB, 4 * GIB).unwrap()
    );
    assert_eq!(
        ResourcePolicy::recommended(Some(256 * GIB), Some(32 * GIB)).unwrap(),
        ResourcePolicy::new(32 * GIB, 8 * GIB).unwrap()
    );
    assert_eq!(
        ResourcePolicy::recommended(Some(u64::MAX), Some(u64::MAX)).unwrap(),
        ResourcePolicy::new(32 * GIB, 8 * GIB).unwrap()
    );
}

#[test]
fn known_gpu_below_the_explicit_minimum_is_rejected_not_silently_clamped() {
    assert_eq!(
        ResourcePolicy::recommended(Some(16 * GIB), Some(2 * GIB)),
        Err(SettingsError::GpuBudgetOutOfBounds {
            actual: 0,
            minimum: MIN_GPU_BUDGET_BYTES,
            maximum: MAX_GPU_BUDGET_BYTES,
        })
    );
}

#[test]
fn temporary_runtime_adapter_uses_the_frozen_percentages() {
    let policy = ResourcePolicy::new(16 * GIB, 8 * GIB).unwrap();
    let adapter = policy.current_runtime_adapter();
    assert_eq!(adapter.cpu_brick_cache_budget_bytes(), 8 * GIB);
    assert_eq!(adapter.cpu_whole_volume_cache_budget_bytes(), 2 * GIB);
    assert_eq!(
        adapter.gpu_brick_cache_budget_bytes(),
        (8_u64 * GIB) * 65 / 100
    );
    assert_eq!(adapter.gpu_dense_cache_budget_bytes(), 8 * GIB / 10);
    assert_eq!(
        adapter.cpu_brick_cache_budget_bytes() + adapter.cpu_whole_volume_cache_budget_bytes(),
        10 * GIB
    );
    assert!(
        adapter.gpu_brick_cache_budget_bytes() + adapter.gpu_dense_cache_budget_bytes()
            <= (8_u64 * GIB) * 75 / 100
    );
}

#[test]
fn settings_document_has_the_exact_closed_shape_and_round_trips() {
    let document = SettingsDocument::new(ResourcePolicy::new(12 * GIB, 6 * GIB).unwrap());
    let encoded = document.to_json_pretty().unwrap();
    let json: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "schema": "mirante4d-settings",
            "schema_version": 1,
            "resource_policy": {
                "cpu_dataset_budget_bytes": 12_u64 * GIB,
                "gpu_budget_bytes": 6_u64 * GIB,
            }
        })
    );
    assert_eq!(SettingsDocument::from_json(&encoded).unwrap(), document);
}

#[test]
fn settings_document_rejects_unknown_fields_at_every_level() {
    let top_level = format!(
        r#"{{
            "schema":"mirante4d-settings",
            "schema_version":1,
            "resource_policy":{{
                "cpu_dataset_budget_bytes":{},
                "gpu_budget_bytes":{}
            }},
            "legacy":true
        }}"#,
        4 * GIB,
        GIB
    );
    let nested = format!(
        r#"{{
            "schema":"mirante4d-settings",
            "schema_version":1,
            "resource_policy":{{
                "cpu_dataset_budget_bytes":{},
                "gpu_budget_bytes":{},
                "volume_cache_budget_bytes":1
            }}
        }}"#,
        4 * GIB,
        GIB
    );
    assert!(matches!(
        SettingsDocument::from_json(&top_level),
        Err(SettingsError::InvalidDocument { .. })
    ));
    assert!(matches!(
        SettingsDocument::from_json(&nested),
        Err(SettingsError::InvalidDocument { .. })
    ));
}

#[test]
fn settings_document_rejects_wrong_identity_version_and_budgets() {
    let wrong_schema = format!(
        r#"{{"schema":"mirante4d-preferences","schema_version":1,"resource_policy":{{"cpu_dataset_budget_bytes":{},"gpu_budget_bytes":{}}}}}"#,
        4 * GIB,
        GIB
    );
    assert!(matches!(
        SettingsDocument::from_json(&wrong_schema),
        Err(SettingsError::UnsupportedSchema { .. })
    ));

    let wrong_version = format!(
        r#"{{"schema":"mirante4d-settings","schema_version":2,"resource_policy":{{"cpu_dataset_budget_bytes":{},"gpu_budget_bytes":{}}}}}"#,
        4 * GIB,
        GIB
    );
    assert_eq!(
        SettingsDocument::from_json(&wrong_version),
        Err(SettingsError::UnsupportedSchemaVersion {
            actual: 2,
            expected: 1,
        })
    );

    let invalid_budget = format!(
        r#"{{"schema":"mirante4d-settings","schema_version":1,"resource_policy":{{"cpu_dataset_budget_bytes":1,"gpu_budget_bytes":{}}}}}"#,
        GIB
    );
    assert!(matches!(
        SettingsDocument::from_json(&invalid_budget),
        Err(SettingsError::CpuDatasetBudgetOutOfBounds { .. })
    ));
}

#[test]
fn linux_path_uses_xdg_then_home_and_never_names_preferences() {
    assert_eq!(
        linux_settings_path(
            Some(OsString::from("/xdg")),
            Some(OsString::from("/home/researcher"))
        )
        .unwrap(),
        PathBuf::from("/xdg/mirante4d/settings.json")
    );
    assert_eq!(
        linux_settings_path(None, Some(OsString::from("/home/researcher"))).unwrap(),
        PathBuf::from("/home/researcher/.config/mirante4d/settings.json")
    );
    assert_eq!(
        linux_settings_path(Some(OsString::new()), Some(OsString::from("/home/r"))).unwrap(),
        PathBuf::from("/home/r/.config/mirante4d/settings.json")
    );
    assert_eq!(
        linux_settings_path(None, None),
        Err(SettingsError::SettingsPathUnavailable)
    );
}

#[test]
fn missing_and_invalid_files_keep_defaults_active_without_writing() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let defaults = SettingsDocument::default();

    let missing = load_document(&path, defaults);
    assert!(matches!(
        missing,
        SettingsLoadOutcome::DefaultsActiveMissing { document } if document == defaults
    ));
    assert!(!path.exists());

    let invalid_bytes = b"{ not valid settings }\n";
    fs::write(&path, invalid_bytes).unwrap();
    let invalid = load_document(&path, defaults);
    assert!(matches!(
        invalid,
        SettingsLoadOutcome::DefaultsActiveRejected { document, .. } if document == defaults
    ));
    assert_eq!(fs::read(&path).unwrap(), invalid_bytes);
}

#[test]
fn oversized_file_is_rejected_without_reading_or_rewriting_it_unboundedly() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let oversized = vec![b'x'; MAX_SETTINGS_DOCUMENT_BYTES + 1];
    fs::write(&path, &oversized).unwrap();

    let outcome = load_document(&path, SettingsDocument::default());
    assert!(matches!(
        outcome,
        SettingsLoadOutcome::DefaultsActiveRejected {
            error: SettingsError::DocumentTooLarge {
                maximum: MAX_SETTINGS_DOCUMENT_BYTES
            },
            ..
        }
    ));
    assert_eq!(fs::read(&path).unwrap(), oversized);
}

#[test]
fn unknown_field_file_is_rejected_and_preserved_byte_for_byte() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let unknown = format!(
        "{{\"schema\":\"mirante4d-settings\",\"schema_version\":1,\"resource_policy\":{{\"cpu_dataset_budget_bytes\":{},\"gpu_budget_bytes\":{},\"legacy_cache\":7}}}}\n",
        4 * GIB,
        GIB
    );
    fs::write(&path, unknown.as_bytes()).unwrap();

    let outcome = load_document(&path, SettingsDocument::default());
    assert!(matches!(
        outcome,
        SettingsLoadOutcome::DefaultsActiveRejected {
            error: SettingsError::InvalidDocument { .. },
            ..
        }
    ));
    assert_eq!(fs::read(&path).unwrap(), unknown.as_bytes());
}

#[test]
fn valid_file_loads_without_rewriting_its_bytes() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let document = SettingsDocument::new(ResourcePolicy::new(10 * GIB, 5 * GIB).unwrap());
    let bytes = format!("{}\n", document.to_json_pretty().unwrap()).into_bytes();
    fs::write(&path, &bytes).unwrap();

    let outcome = load_document(&path, SettingsDocument::default());
    assert!(matches!(
        outcome,
        SettingsLoadOutcome::Loaded { document: loaded } if loaded == document
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn background_actor_loads_and_persists_with_restart_required_event() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("config/settings.json");
    let actor = SettingsActor::spawn(path.clone(), SettingsDocument::default()).unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::Loaded(SettingsLoadOutcome::DefaultsActiveMissing { .. })
    ));

    let document = SettingsDocument::new(ResourcePolicy::new(6 * GIB, 3 * GIB).unwrap());
    let request_id = SettingsRequestId::new(42);
    actor
        .request_save(request_id, document, RejectedFileDisposition::Preserve)
        .unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SavePending { request_id: observed } if observed == request_id
    ));
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SavePersisted {
            request_id: observed,
            document: observed_document,
            restart_required: true,
        } if observed == request_id && observed_document == document
    ));

    assert_eq!(
        SettingsDocument::from_json(&fs::read_to_string(&path).unwrap()).unwrap(),
        document
    );
    assert_no_owned_temporaries(path.parent().unwrap());
    actor.shutdown().unwrap();
}

#[test]
fn rejected_file_requires_explicit_replacement_and_preserves_bytes() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let rejected_bytes = b"legacy or corrupt settings\n";
    fs::write(&path, rejected_bytes).unwrap();
    let actor = SettingsActor::spawn(path.clone(), SettingsDocument::default()).unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::Loaded(SettingsLoadOutcome::DefaultsActiveRejected { .. })
    ));

    let replacement = SettingsDocument::new(ResourcePolicy::new(8 * GIB, 4 * GIB).unwrap());
    actor
        .request_save(
            SettingsRequestId::new(1),
            replacement,
            RejectedFileDisposition::Preserve,
        )
        .unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SavePending { .. }
    ));
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SaveRejected {
            error: SettingsError::ExplicitReplacementRequired,
            ..
        }
    ));
    assert_eq!(fs::read(&path).unwrap(), rejected_bytes);

    actor
        .request_save(
            SettingsRequestId::new(2),
            replacement,
            RejectedFileDisposition::ReplaceExplicitly,
        )
        .unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SavePending { .. }
    ));
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SavePersisted {
            request_id,
            document,
            ..
        } if request_id == SettingsRequestId::new(2) && document == replacement
    ));
    assert_eq!(
        SettingsDocument::from_json(&fs::read_to_string(&path).unwrap()).unwrap(),
        replacement
    );
    actor.shutdown().unwrap();
}

#[test]
fn file_that_becomes_invalid_after_startup_is_not_silently_overwritten() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let actor = SettingsActor::spawn(path.clone(), SettingsDocument::default()).unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::Loaded(SettingsLoadOutcome::DefaultsActiveMissing { .. })
    ));

    let invalid_bytes = b"externally changed invalid settings\n";
    fs::write(&path, invalid_bytes).unwrap();
    actor
        .request_save(
            SettingsRequestId::new(9),
            SettingsDocument::new(ResourcePolicy::new(8 * GIB, 4 * GIB).unwrap()),
            RejectedFileDisposition::Preserve,
        )
        .unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SavePending {
            request_id
        } if request_id == SettingsRequestId::new(9)
    ));
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::SaveRejected {
            request_id,
            error: SettingsError::ExplicitReplacementRequired,
        } if request_id == SettingsRequestId::new(9)
    ));
    assert_eq!(fs::read(&path).unwrap(), invalid_bytes);
    actor.shutdown().unwrap();
}

#[test]
fn actor_never_inspects_or_changes_legacy_preferences_file() {
    let tempdir = tempfile::tempdir().unwrap();
    let config_dir = tempdir.path().join("mirante4d");
    fs::create_dir_all(&config_dir).unwrap();
    let legacy_path = config_dir.join("preferences.json");
    let settings_path = config_dir.join("settings.json");
    let legacy_bytes = b"confidential legacy sentinel\n";
    fs::write(&legacy_path, legacy_bytes).unwrap();

    let actor = SettingsActor::spawn(settings_path.clone(), SettingsDocument::default()).unwrap();
    assert!(matches!(
        receive_event(&actor),
        SettingsEvent::Loaded(SettingsLoadOutcome::DefaultsActiveMissing { .. })
    ));
    actor.shutdown().unwrap();

    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);
    assert!(!settings_path.exists());
}

#[test]
fn forced_commit_failure_preserves_prior_valid_file_and_cleans_its_temporary() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let prior = SettingsDocument::new(ResourcePolicy::new(4 * GIB, 2 * GIB).unwrap());
    let prior_bytes = format!("{}\n", prior.to_json_pretty().unwrap()).into_bytes();
    fs::write(&path, &prior_bytes).unwrap();
    let replacement = SettingsDocument::new(ResourcePolicy::new(12 * GIB, 6 * GIB).unwrap());

    let error = save_document_atomically_with_commit(&path, replacement, |_temporary, _target| {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
    })
    .unwrap_err();
    assert_eq!(
        error,
        SettingsError::Io {
            stage: SettingsIoStage::CommitReplacement,
            kind: io::ErrorKind::PermissionDenied,
        }
    );
    assert_eq!(fs::read(&path).unwrap(), prior_bytes);
    assert_no_owned_temporaries(tempdir.path());
}

#[test]
fn directory_sync_failure_after_replace_is_typed_as_commit_indeterminate() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let prior = SettingsDocument::new(ResourcePolicy::new(4 * GIB, 2 * GIB).unwrap());
    fs::write(&path, format!("{}\n", prior.to_json_pretty().unwrap())).unwrap();
    let replacement = SettingsDocument::new(ResourcePolicy::new(12 * GIB, 6 * GIB).unwrap());

    let error = save_document_atomically_with_commit_and_sync(
        &path,
        replacement,
        |temporary, target| fs::rename(temporary, target),
        |_parent| {
            Err(SettingsError::Io {
                stage: SettingsIoStage::SyncDirectory,
                kind: io::ErrorKind::PermissionDenied,
            })
        },
    )
    .unwrap_err();

    assert_eq!(
        error,
        SettingsError::CommitIndeterminate {
            kind: io::ErrorKind::PermissionDenied,
        }
    );
    assert_eq!(
        SettingsDocument::from_json(&fs::read_to_string(&path).unwrap()).unwrap(),
        replacement
    );
    assert_no_owned_temporaries(tempdir.path());
}

#[test]
fn temporary_names_are_unique_and_created_in_the_target_directory() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let (first_path, first) = create_unique_temporary(&path).unwrap();
    let (second_path, second) = create_unique_temporary(&path).unwrap();
    assert_ne!(first_path, second_path);
    assert_eq!(first_path.parent(), Some(tempdir.path()));
    assert_eq!(second_path.parent(), Some(tempdir.path()));
    drop(first);
    drop(second);
    fs::remove_file(first_path).unwrap();
    fs::remove_file(second_path).unwrap();
}

#[test]
fn bounded_event_backpressure_cannot_deadlock_joined_shutdown() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("settings.json");
    let actor = SettingsActor::spawn(path, SettingsDocument::default()).unwrap();
    let document = SettingsDocument::default();

    for request in 0..(ACTOR_QUEUE_CAPACITY * 2 + 1) as u64 {
        loop {
            match actor.request_save(
                SettingsRequestId::new(request),
                document,
                RejectedFileDisposition::Preserve,
            ) {
                Ok(()) => break,
                Err(SettingsError::ActorQueueFull) => std::thread::yield_now(),
                Err(error) => panic!("unexpected request error: {error}"),
            }
        }
    }

    let (finished_sender, finished_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = finished_sender.send(actor.shutdown());
    });
    finished_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("bounded event backpressure must not deadlock shutdown")
        .unwrap();
}
