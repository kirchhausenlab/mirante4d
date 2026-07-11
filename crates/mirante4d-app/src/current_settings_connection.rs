//! Composition-only connection between application settings intent and the
//! background settings actor.

use std::{io, path::PathBuf};

use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, ResourcePolicyPersistenceOutcome,
    ResourcePolicyRejection, SettingsChangeToken,
};
use mirante4d_settings::{
    ResourcePolicy, SettingsActor, SettingsDocument, SettingsError, SettingsEvent, SettingsIoStage,
    SettingsLoadOutcome, SettingsRequestId, default_linux_settings_path,
    recommended_for_current_system,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsStartupStatus {
    Loaded,
    DefaultsActiveMissing,
    DefaultsActiveRejected(ResourcePolicyRejection),
    Unavailable(ResourcePolicyRejection),
}

pub(crate) struct CurrentSettingsConnection {
    actor: Option<SettingsActor>,
    startup_status: SettingsStartupStatus,
    rejected_file_present: bool,
    pending: Option<SettingsChangeToken>,
}

impl CurrentSettingsConnection {
    /// Starts settings before UI construction. Filesystem work runs on the
    /// actor; this composition call only waits for its typed initial outcome.
    pub(crate) fn start() -> (Self, ResourcePolicy) {
        let defaults = recommended_for_current_system(None).unwrap_or_default();
        let path = match default_linux_settings_path() {
            Ok(path) => path,
            Err(error) => {
                let rejection = map_settings_error(&error);
                return (Self::unavailable(rejection), defaults);
            }
        };
        Self::start_with_path(path, defaults)
    }

    fn start_with_path(path: PathBuf, defaults: ResourcePolicy) -> (Self, ResourcePolicy) {
        let mut actor = match SettingsActor::spawn(path, SettingsDocument::new(defaults)) {
            Ok(actor) => actor,
            Err(error) => {
                let rejection = map_settings_error(&error);
                return (Self::unavailable(rejection), defaults);
            }
        };
        match actor.receive_startup() {
            Ok(SettingsLoadOutcome::Loaded { document }) => (
                Self {
                    actor: Some(actor),
                    startup_status: SettingsStartupStatus::Loaded,
                    rejected_file_present: false,
                    pending: None,
                },
                document.resource_policy(),
            ),
            Ok(SettingsLoadOutcome::DefaultsActiveMissing { document }) => (
                Self {
                    actor: Some(actor),
                    startup_status: SettingsStartupStatus::DefaultsActiveMissing,
                    rejected_file_present: false,
                    pending: None,
                },
                document.resource_policy(),
            ),
            Ok(SettingsLoadOutcome::DefaultsActiveRejected { document, error }) => (
                Self {
                    actor: Some(actor),
                    startup_status: SettingsStartupStatus::DefaultsActiveRejected(
                        map_settings_error(&error),
                    ),
                    rejected_file_present: true,
                    pending: None,
                },
                document.resource_policy(),
            ),
            Err(error) => {
                let rejection = map_settings_error(&error);
                let _ = actor.shutdown();
                (Self::unavailable(rejection), defaults)
            }
        }
    }

    pub(crate) const fn startup_status(&self) -> SettingsStartupStatus {
        self.startup_status
    }

    pub(crate) const fn rejected_file_present(&self) -> bool {
        self.rejected_file_present
    }

    pub(crate) const fn pending(&self) -> Option<SettingsChangeToken> {
        self.pending
    }

    /// Handles one application event and returns a typed completion command if
    /// the actor could not accept the request.
    pub(crate) fn observe_application_event(
        &mut self,
        event: &ApplicationEvent,
    ) -> Option<ApplicationCommand> {
        let ApplicationEvent::ResourcePolicyChangePending { token } = event else {
            return None;
        };
        if self.pending == Some(*token) {
            return None;
        }
        if self.pending.is_some() {
            return Some(rejected_command(
                *token,
                ResourcePolicyRejection::ActorQueueFull,
            ));
        }
        let Some(actor) = self
            .actor
            .as_ref()
            .filter(|_| !matches!(self.startup_status, SettingsStartupStatus::Unavailable(_)))
        else {
            return Some(rejected_command(
                *token,
                ResourcePolicyRejection::ActorUnavailable,
            ));
        };
        let request_id = SettingsRequestId::new(token.id().get());
        match actor.request_save(
            request_id,
            SettingsDocument::new(token.policy()),
            token.rejected_file_disposition(),
        ) {
            Ok(()) => {
                self.pending = Some(*token);
                None
            }
            Err(error) => Some(rejected_command(*token, map_settings_error(&error))),
        }
    }

    /// Drains the bounded actor event queue into application completion
    /// commands. Correlation checks include both request ID and policy.
    pub(crate) fn poll(&mut self) -> Vec<ApplicationCommand> {
        let mut commands = Vec::new();
        if self.actor.is_none()
            || matches!(self.startup_status, SettingsStartupStatus::Unavailable(_))
        {
            return commands;
        }
        loop {
            let event = match self
                .actor
                .as_ref()
                .expect("actor presence was checked")
                .try_recv()
            {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => {
                    if let Some(token) = self.pending.take() {
                        commands.push(rejected_command(
                            token,
                            ResourcePolicyRejection::ActorUnavailable,
                        ));
                    }
                    self.mark_actor_unavailable();
                    break;
                }
            };
            match event {
                SettingsEvent::Loaded(_) => {
                    if let Some(token) = self.pending.take() {
                        commands.push(rejected_command(
                            token,
                            ResourcePolicyRejection::ActorUnavailable,
                        ));
                    }
                    self.mark_actor_unavailable();
                    break;
                }
                SettingsEvent::SavePending { request_id } => {
                    if !self.request_matches(request_id) {
                        if let Some(token) = self.pending.take() {
                            commands.push(rejected_command(
                                token,
                                ResourcePolicyRejection::ActorUnavailable,
                            ));
                        }
                        self.mark_actor_unavailable();
                        break;
                    }
                }
                SettingsEvent::SavePersisted {
                    request_id,
                    document,
                    restart_required,
                } => {
                    let Some(token) = self.take_matching(request_id, document.resource_policy())
                    else {
                        if let Some(token) = self.pending.take() {
                            commands.push(rejected_command(
                                token,
                                ResourcePolicyRejection::ActorUnavailable,
                            ));
                        }
                        self.mark_actor_unavailable();
                        break;
                    };
                    if !restart_required {
                        commands.push(rejected_command(
                            token,
                            ResourcePolicyRejection::ActorUnavailable,
                        ));
                        self.mark_actor_unavailable();
                        break;
                    }
                    self.rejected_file_present = false;
                    self.startup_status = SettingsStartupStatus::Loaded;
                    commands.push(ApplicationCommand::CompleteResourcePolicyPersistence {
                        token,
                        outcome: ResourcePolicyPersistenceOutcome::Persisted,
                    });
                }
                SettingsEvent::SaveRejected { request_id, error } => {
                    let Some(token) = self.take_matching_id(request_id) else {
                        if let Some(token) = self.pending.take() {
                            commands.push(rejected_command(
                                token,
                                ResourcePolicyRejection::ActorUnavailable,
                            ));
                        }
                        self.mark_actor_unavailable();
                        break;
                    };
                    if matches!(
                        &error,
                        SettingsError::ExplicitReplacementRequired
                            | SettingsError::CommitIndeterminate { .. }
                    ) {
                        self.rejected_file_present = true;
                    }
                    commands.push(rejected_command(token, map_settings_error(&error)));
                }
            }
        }
        commands
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), SettingsError> {
        self.pending = None;
        match self.actor.take() {
            Some(actor) => actor.shutdown(),
            None => Ok(()),
        }
    }

    fn unavailable(rejection: ResourcePolicyRejection) -> Self {
        Self {
            actor: None,
            startup_status: SettingsStartupStatus::Unavailable(rejection),
            rejected_file_present: false,
            pending: None,
        }
    }

    fn mark_actor_unavailable(&mut self) {
        self.startup_status =
            SettingsStartupStatus::Unavailable(ResourcePolicyRejection::ActorUnavailable);
    }

    fn request_matches(&self, request_id: SettingsRequestId) -> bool {
        self.pending
            .is_some_and(|token| token.id().get() == request_id.get())
    }

    fn take_matching_id(&mut self, request_id: SettingsRequestId) -> Option<SettingsChangeToken> {
        if self.request_matches(request_id) {
            self.pending.take()
        } else {
            None
        }
    }

    fn take_matching(
        &mut self,
        request_id: SettingsRequestId,
        policy: ResourcePolicy,
    ) -> Option<SettingsChangeToken> {
        if self
            .pending
            .is_some_and(|token| token.id().get() == request_id.get() && token.policy() == policy)
        {
            self.pending.take()
        } else {
            None
        }
    }
}

fn rejected_command(
    token: SettingsChangeToken,
    reason: ResourcePolicyRejection,
) -> ApplicationCommand {
    ApplicationCommand::CompleteResourcePolicyPersistence {
        token,
        outcome: ResourcePolicyPersistenceOutcome::Rejected(reason),
    }
}

fn map_settings_error(error: &SettingsError) -> ResourcePolicyRejection {
    match error {
        SettingsError::CpuDatasetBudgetOutOfBounds { .. }
        | SettingsError::GpuBudgetOutOfBounds { .. } => ResourcePolicyRejection::InvalidPolicy,
        SettingsError::UnsupportedSchema { .. }
        | SettingsError::UnsupportedSchemaVersion { .. } => {
            ResourcePolicyRejection::UnsupportedDocument
        }
        SettingsError::InvalidDocument { .. } => ResourcePolicyRejection::InvalidDocument,
        SettingsError::DocumentTooLarge { .. } => ResourcePolicyRejection::DocumentTooLarge,
        SettingsError::SettingsPathUnavailable => ResourcePolicyRejection::PathUnavailable,
        SettingsError::Io { stage, kind } => map_io_error(*stage, *kind),
        SettingsError::CommitIndeterminate { .. } => ResourcePolicyRejection::CommitIndeterminate,
        SettingsError::ActorQueueFull => ResourcePolicyRejection::ActorQueueFull,
        SettingsError::ExplicitReplacementRequired => {
            ResourcePolicyRejection::ExplicitReplacementRequired
        }
        SettingsError::ActorUnavailable
        | SettingsError::ActorEventChannelDisconnected
        | SettingsError::StartupAlreadyReceived
        | SettingsError::StartupNotReceived
        | SettingsError::UnexpectedStartupEvent
        | SettingsError::ActorThreadPanicked => ResourcePolicyRejection::ActorUnavailable,
    }
}

fn map_io_error(stage: SettingsIoStage, kind: io::ErrorKind) -> ResourcePolicyRejection {
    if kind == io::ErrorKind::PermissionDenied {
        return ResourcePolicyRejection::PermissionDenied;
    }
    match stage {
        SettingsIoStage::Read => ResourcePolicyRejection::ReadFailed,
        SettingsIoStage::SyncDirectory => ResourcePolicyRejection::CommitIndeterminate,
        SettingsIoStage::SpawnActor => ResourcePolicyRejection::ActorUnavailable,
        SettingsIoStage::CreateDirectory
        | SettingsIoStage::CreateTemporary
        | SettingsIoStage::WriteTemporary
        | SettingsIoStage::SyncTemporary
        | SettingsIoStage::ReadBackTemporary
        | SettingsIoStage::CommitReplacement
        | SettingsIoStage::RemoveTemporary => ResourcePolicyRejection::AtomicWriteFailed,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs, thread,
        time::{Duration, Instant},
    };

    use mirante4d_application::{
        ApplicationState, MAX_PENDING_EVENTS, SourceSessionGeneration, UnboundWorkspace,
    };
    use mirante4d_dataset::{
        DatasetCatalog, DatasetLayer, DatasetSourceId, ScientificIdentityStatus,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, GridToWorld, IntensityDType, IsoLightState,
        LayerTransfer, LogicalLayerKey, Opacity, Projection, RenderState, RgbColor, SamplingPolicy,
        Shape4D, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_project_model::{LayerViewState, ProjectId, ViewState};
    use mirante4d_settings::{GIB, RejectedFileDisposition};
    use tempfile::tempdir;

    use super::*;

    fn view() -> ViewState {
        let transfer = LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        );
        ViewState::new(
            vec![LayerViewState::new(
                LogicalLayerKey::new(0),
                true,
                transfer,
                RenderState::mip(SamplingPolicy::SmoothLinear),
            )],
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::origin(),
                UnitQuaternion::identity(),
                1.0,
                320.0,
                10.0,
            )
            .unwrap(),
            ViewerLayout::Single3d,
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap()
    }

    fn application() -> ApplicationState {
        let catalog = DatasetCatalog::new(
            "settings-connection-test",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
            vec![
                DatasetLayer::new(
                    LogicalLayerKey::new(0),
                    "layer-0",
                    Shape4D::new(1, 1, 1, 1).unwrap(),
                    IntensityDType::Uint16,
                    GridToWorld::identity(),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let workspace =
            UnboundWorkspace::new(ProjectId::from_bytes([7; 16]), view(), Vec::new()).unwrap();
        ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            catalog,
            workspace,
            ResourcePolicy::default(),
        )
        .unwrap()
    }

    fn settings_tokens(
        requests: &[(ResourcePolicy, RejectedFileDisposition)],
    ) -> Vec<SettingsChangeToken> {
        let mut application = application();
        let mut tokens = Vec::with_capacity(requests.len());
        for (policy, disposition) in requests {
            application
                .dispatch(ApplicationCommand::RequestResourcePolicyChange {
                    policy: *policy,
                    rejected_file_disposition: *disposition,
                })
                .unwrap();
            let token = application
                .drain_events(MAX_PENDING_EVENTS)
                .into_iter()
                .find_map(|event| match event {
                    ApplicationEvent::ResourcePolicyChangePending { token } => Some(token),
                    _ => None,
                })
                .expect("resource-policy pending event");
            application
                .dispatch(ApplicationCommand::CompleteResourcePolicyPersistence {
                    token,
                    outcome: ResourcePolicyPersistenceOutcome::Rejected(
                        ResourcePolicyRejection::ActorUnavailable,
                    ),
                })
                .unwrap();
            application.drain_events(MAX_PENDING_EVENTS);
            tokens.push(token);
        }
        tokens
    }

    fn pending_event(token: SettingsChangeToken) -> ApplicationEvent {
        ApplicationEvent::ResourcePolicyChangePending { token }
    }

    fn wait_for_command(connection: &mut CurrentSettingsConnection) -> ApplicationCommand {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut commands = connection.poll();
            if !commands.is_empty() {
                assert_eq!(commands.len(), 1);
                return commands.remove(0);
            }
            assert!(
                Instant::now() < deadline,
                "settings actor command timed out"
            );
            thread::yield_now();
        }
    }

    fn assert_completion(
        command: ApplicationCommand,
        expected_token: SettingsChangeToken,
        expected_outcome: ResourcePolicyPersistenceOutcome,
    ) {
        match command {
            ApplicationCommand::CompleteResourcePolicyPersistence { token, outcome } => {
                assert_eq!(token, expected_token);
                assert_eq!(outcome, expected_outcome);
            }
            command => panic!("unexpected settings command: {command:?}"),
        }
    }

    #[test]
    fn startup_reports_loaded_missing_and_rejected_without_rewriting() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let defaults = ResourcePolicy::new(8 * GIB, 2 * GIB).unwrap();

        let (mut missing, active) =
            CurrentSettingsConnection::start_with_path(path.clone(), defaults);
        assert_eq!(active, defaults);
        assert_eq!(
            missing.startup_status(),
            SettingsStartupStatus::DefaultsActiveMissing
        );
        assert!(!missing.rejected_file_present());
        assert!(!path.exists());
        missing.shutdown().unwrap();

        let loaded_policy = ResourcePolicy::new(10 * GIB, 3 * GIB).unwrap();
        let loaded_bytes = format!(
            "{}\n",
            SettingsDocument::new(loaded_policy)
                .to_json_pretty()
                .unwrap()
        )
        .into_bytes();
        fs::write(&path, &loaded_bytes).unwrap();
        let (mut loaded, active) =
            CurrentSettingsConnection::start_with_path(path.clone(), defaults);
        assert_eq!(active, loaded_policy);
        assert_eq!(loaded.startup_status(), SettingsStartupStatus::Loaded);
        assert!(!loaded.rejected_file_present());
        assert_eq!(fs::read(&path).unwrap(), loaded_bytes);
        loaded.shutdown().unwrap();

        let rejected_bytes = b"not valid settings\n";
        fs::write(&path, rejected_bytes).unwrap();
        let (mut rejected, active) =
            CurrentSettingsConnection::start_with_path(path.clone(), defaults);
        assert_eq!(active, defaults);
        assert!(matches!(
            rejected.startup_status(),
            SettingsStartupStatus::DefaultsActiveRejected(ResourcePolicyRejection::InvalidDocument)
        ));
        assert!(rejected.rejected_file_present());
        assert_eq!(fs::read(&path).unwrap(), rejected_bytes);
        rejected.shutdown().unwrap();
    }

    #[test]
    fn rejected_file_is_preserved_until_exact_explicit_replacement() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let rejected_bytes = b"legacy or corrupt settings\n";
        fs::write(&path, rejected_bytes).unwrap();
        let (mut connection, _) =
            CurrentSettingsConnection::start_with_path(path.clone(), ResourcePolicy::default());
        let preserve_policy = ResourcePolicy::new(8 * GIB, 2 * GIB).unwrap();
        let replace_policy = ResourcePolicy::new(10 * GIB, 3 * GIB).unwrap();
        let tokens = settings_tokens(&[
            (preserve_policy, RejectedFileDisposition::Preserve),
            (replace_policy, RejectedFileDisposition::ReplaceExplicitly),
        ]);

        assert!(
            connection
                .observe_application_event(&pending_event(tokens[0]))
                .is_none()
        );
        assert_eq!(connection.pending(), Some(tokens[0]));
        assert_completion(
            wait_for_command(&mut connection),
            tokens[0],
            ResourcePolicyPersistenceOutcome::Rejected(
                ResourcePolicyRejection::ExplicitReplacementRequired,
            ),
        );
        assert_eq!(connection.pending(), None);
        assert!(connection.rejected_file_present());
        assert_eq!(fs::read(&path).unwrap(), rejected_bytes);

        assert!(
            connection
                .observe_application_event(&pending_event(tokens[1]))
                .is_none()
        );
        assert_eq!(connection.pending(), Some(tokens[1]));
        assert_completion(
            wait_for_command(&mut connection),
            tokens[1],
            ResourcePolicyPersistenceOutcome::Persisted,
        );
        assert_eq!(connection.pending(), None);
        assert!(!connection.rejected_file_present());
        assert_eq!(connection.startup_status(), SettingsStartupStatus::Loaded);
        assert_eq!(
            SettingsDocument::from_json(&fs::read_to_string(&path).unwrap()).unwrap(),
            SettingsDocument::new(replace_policy)
        );
        connection.shutdown().unwrap();
    }

    #[test]
    fn correlation_never_consumes_or_completes_a_different_pending_token() {
        let first_policy = ResourcePolicy::new(8 * GIB, 2 * GIB).unwrap();
        let second_policy = ResourcePolicy::new(10 * GIB, 3 * GIB).unwrap();
        let tokens = settings_tokens(&[
            (first_policy, RejectedFileDisposition::Preserve),
            (second_policy, RejectedFileDisposition::ReplaceExplicitly),
        ]);
        let mut connection =
            CurrentSettingsConnection::unavailable(ResourcePolicyRejection::ActorUnavailable);
        connection.pending = Some(tokens[0]);

        assert_eq!(
            connection.take_matching(SettingsRequestId::new(tokens[1].id().get()), first_policy),
            None
        );
        assert_eq!(connection.pending(), Some(tokens[0]));
        assert_eq!(
            connection.take_matching(SettingsRequestId::new(tokens[0].id().get()), second_policy),
            None
        );
        assert_eq!(connection.pending(), Some(tokens[0]));
        assert_eq!(
            connection.take_matching(SettingsRequestId::new(tokens[0].id().get()), first_policy),
            Some(tokens[0])
        );
        assert_eq!(connection.pending(), None);

        connection.pending = Some(tokens[0]);
        assert!(
            connection
                .observe_application_event(&pending_event(tokens[0]))
                .is_none()
        );
        assert_eq!(connection.pending(), Some(tokens[0]));
        let command = connection
            .observe_application_event(&pending_event(tokens[1]))
            .expect("second pending request is bounded");
        assert_completion(
            command,
            tokens[1],
            ResourcePolicyPersistenceOutcome::Rejected(ResourcePolicyRejection::ActorQueueFull),
        );
        assert_eq!(connection.pending(), Some(tokens[0]));
    }

    #[test]
    fn unavailable_and_bounded_failures_map_to_exact_typed_completions() {
        let policy = ResourcePolicy::new(8 * GIB, 2 * GIB).unwrap();
        let token = settings_tokens(&[(policy, RejectedFileDisposition::Preserve)])[0];
        let mut unavailable =
            CurrentSettingsConnection::unavailable(ResourcePolicyRejection::PathUnavailable);
        assert_completion(
            unavailable
                .observe_application_event(&pending_event(token))
                .unwrap(),
            token,
            ResourcePolicyPersistenceOutcome::Rejected(ResourcePolicyRejection::ActorUnavailable),
        );

        assert_eq!(
            map_settings_error(&SettingsError::ActorQueueFull),
            ResourcePolicyRejection::ActorQueueFull
        );
        assert_eq!(
            map_settings_error(&SettingsError::ExplicitReplacementRequired),
            ResourcePolicyRejection::ExplicitReplacementRequired
        );
        assert_eq!(
            map_io_error(SettingsIoStage::Read, io::ErrorKind::Other),
            ResourcePolicyRejection::ReadFailed
        );
        assert_eq!(
            map_io_error(SettingsIoStage::WriteTemporary, io::ErrorKind::WriteZero),
            ResourcePolicyRejection::AtomicWriteFailed
        );
        assert_eq!(
            map_io_error(SettingsIoStage::SyncDirectory, io::ErrorKind::Other),
            ResourcePolicyRejection::CommitIndeterminate
        );
        assert_eq!(
            map_io_error(SettingsIoStage::Read, io::ErrorKind::PermissionDenied),
            ResourcePolicyRejection::PermissionDenied
        );
    }

    #[test]
    fn explicit_shutdown_joins_and_is_idempotent() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let (mut connection, _) =
            CurrentSettingsConnection::start_with_path(path, ResourcePolicy::default());
        assert!(connection.actor.is_some());
        connection.shutdown().unwrap();
        assert!(connection.actor.is_none());
        assert_eq!(connection.pending(), None);
        connection.shutdown().unwrap();
    }
}
