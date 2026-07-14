//! Product orchestration for the two exact WP-12 analysis operations.

use anyhow::{Context, Result, anyhow, bail, ensure};
use mirante4d_analysis_core::AnalysisDefinition;
use mirante4d_analysis_runtime::{AnalysisFailure, CompletionEvent, required_result_bytes};
use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, OperationCompletion, OperationFailureCode, OperationKind,
    OperationToken, ProjectStoreLifecycle, SourceVerificationSnapshot, WorkspaceSnapshot,
};
use mirante4d_dataset::ResourceRegion;
use mirante4d_dataset_runtime::{RequestPriority, RuntimeFault, RuntimeFaultCode};
use mirante4d_domain::Shape3D;
use mirante4d_project_model::ArtifactSchema;

use crate::{
    MiranteWorkbenchApp, application_view, current_egui_shell_bridge,
    dataset_requests::SCOPE_ANALYSIS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductAnalysisScope {
    FullTimeTrace,
    CurrentTimepointBox,
}

impl MiranteWorkbenchApp {
    pub(crate) fn analysis_start_unavailable_reason(&self) -> Option<String> {
        if self.analysis_runtime.active_token().is_some() {
            return Some("An analysis is already running or being saved.".to_owned());
        }
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        if !matches!(snapshot.source(), SourceVerificationSnapshot::Verified(_)) {
            return Some("Verify the microscopy source before analyzing it.".to_owned());
        }
        let WorkspaceSnapshot::Bound { project, dirty, .. } = snapshot.workspace() else {
            return Some("Save the project once before running analysis.".to_owned());
        };
        if *dirty {
            return Some("Save current project changes before running analysis.".to_owned());
        }
        if !snapshot.active_operations().is_empty() {
            return Some("Finish the current background operation first.".to_owned());
        }
        if self.dataset.resource_identity()
            != snapshot.catalog().scientific_identity().resource_identity()
        {
            return Some("The verified microscopy runtime is not ready yet.".to_owned());
        }
        let Some(service) = self.project_store.as_ref() else {
            return Some("Project storage is unavailable.".to_owned());
        };
        let status = service.status();
        if status.lifecycle() != ProjectStoreLifecycle::Established
            || !status.writable()
            || status.writes_suspended()
            || status.foreground_active()
            || status.autosave_active()
            || status.current_manual().is_none()
            || status.project_id() != Some(project.project_id())
        {
            return Some("Wait for a writable project save to become ready.".to_owned());
        }
        None
    }

    pub(crate) fn start_product_analysis(&mut self, scope: ProductAnalysisScope) -> Result<()> {
        if let Some(reason) = self.analysis_start_unavailable_reason() {
            bail!(reason);
        }
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let view = application_view(&snapshot);
        let layer = snapshot
            .catalog()
            .layer(view.active_layer())
            .context("the active microscopy layer is missing")?;
        let definition = match scope {
            ProductAnalysisScope::FullTimeTrace => AnalysisDefinition::full_intensity_summary(
                snapshot.catalog(),
                view.active_layer(),
                0,
                layer.shape().t(),
            ),
            ProductAnalysisScope::CurrentTimepointBox => {
                let time_start = view.timepoint().get();
                let shape = Shape3D::new(
                    self.analysis_runtime.roi_shape()[0],
                    self.analysis_runtime.roi_shape()[1],
                    self.analysis_runtime.roi_shape()[2],
                )?;
                let region = ResourceRegion::new(self.analysis_runtime.roi_origin(), shape)?;
                AnalysisDefinition::box_roi_intensity_statistics(
                    snapshot.catalog(),
                    view.active_layer(),
                    time_start,
                    time_start
                        .checked_add(1)
                        .context("analysis timepoint overflowed")?,
                    region,
                )
            }
        }
        .map_err(|error| anyhow!("analysis input is invalid: {error}"))?;

        let result_bytes = required_result_bytes(&definition)
            .map_err(|error| anyhow!("analysis result reservation is invalid: {error}"))?;
        let generation = self
            .dataset
            .dispatcher_mut()
            .advance_scope(SCOPE_ANALYSIS)
            .map_err(|fault| anyhow!("could not prepare analysis cancellation: {fault}"))?;
        let charge = self
            .dataset
            .dispatcher()
            .try_acquire_analysis_bytes(result_bytes)
            .map_err(|fault| anyhow!("not enough configured analysis memory: {fault}"))?;
        let token = self
            .begin_background_operation(OperationKind::Analysis)
            .context("the application could not begin analysis")?;
        if let Err(error) =
            self.analysis_runtime
                .start(token.clone(), definition, generation, charge)
        {
            self.complete_background_operation(
                token,
                OperationCompletion::Failed(OperationFailureCode::AnalysisInvalidInput),
            );
            return Err(error);
        }
        if let Err(error) = self.pump_analysis_requests() {
            self.abort_running_analysis(
                &token,
                OperationFailureCode::AnalysisExecutionFailed,
                &error,
            );
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn request_analysis_cancel(&mut self) -> Result<()> {
        let token = self
            .analysis_runtime
            .active_token()
            .context("no analysis is active")?
            .clone();
        current_egui_shell_bridge::dispatch(
            &mut self.application,
            ApplicationCommand::CancelOperation(token.operation_id()),
        )
        .map_err(|fault| anyhow!("analysis cancellation was rejected: {fault:?}"))?;
        self.pump_application_services();
        Ok(())
    }

    pub(crate) fn observe_analysis_application_event(&mut self, event: &ApplicationEvent) {
        match event {
            ApplicationEvent::AnalysisCommitRequested { token, projection } => {
                let mut sources = match self.analysis_runtime.take_staged_object_sources(token) {
                    Ok(sources) => sources,
                    Err(error) => {
                        tracing::error!(%error, "analysis artifact sources were unavailable");
                        let _ = self.analysis_runtime.drop_commit(token);
                        self.complete_background_operation(
                            token.clone(),
                            OperationCompletion::Failed(
                                OperationFailureCode::AnalysisExecutionFailed,
                            ),
                        );
                        return;
                    }
                };
                let snapshot = current_egui_shell_bridge::snapshot(&self.application);
                if let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() {
                    sources.retain(|source| {
                        !project
                            .artifacts()
                            .iter()
                            .any(|artifact| artifact.object() == source.descriptor())
                    });
                }
                let request = self
                    .project_store
                    .as_mut()
                    .ok_or_else(|| anyhow!("project storage is unavailable"))
                    .and_then(|service| {
                        service
                            .submit_analysis_commit(
                                &snapshot,
                                token.clone(),
                                projection.as_ref().clone(),
                                sources,
                            )
                            .map_err(|error| {
                                anyhow!("analysis project save was rejected: {error:?}")
                            })
                    });
                if let Err(error) = request {
                    let _ = self.analysis_runtime.drop_commit(token);
                    self.complete_background_operation(
                        token.clone(),
                        OperationCompletion::Failed(OperationFailureCode::AnalysisExecutionFailed),
                    );
                    self.project_status_message =
                        Some(format!("Analysis could not be saved: {error}"));
                }
            }
            ApplicationEvent::OperationCancellationRequested { token }
                if token.kind() == OperationKind::Analysis =>
            {
                self.cancel_analysis_worker_for_event(token);
            }
            ApplicationEvent::CurrentSourceReplaced { .. } => {
                self.cancel_pending_analysis_artifact_load();
                self.analysis_runtime.clear_loaded();
            }
            _ => {}
        }
    }

    pub(crate) fn request_current_analysis_artifacts(&mut self) {
        self.cancel_pending_analysis_artifact_load();
        self.analysis_runtime.clear_loaded();
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let WorkspaceSnapshot::Bound { project, .. } = snapshot.workspace() else {
            return;
        };
        let handles = project
            .artifacts()
            .iter()
            .filter(|artifact| {
                matches!(
                    artifact.schema(),
                    ArtifactSchema::AnalysisTableV1 | ArtifactSchema::AnalysisPlotV1
                )
            })
            .map(|artifact| artifact.handle_id().clone())
            .collect::<Vec<_>>();
        if handles.is_empty() {
            return;
        }
        let Some(generation_id) = self
            .project_store
            .as_ref()
            .and_then(|service| service.status().current_manual())
        else {
            self.project_status_message = Some(
                "Saved analysis values could not be located for this project head.".to_owned(),
            );
            return;
        };
        let request = self
            .project_store
            .as_mut()
            .context("project storage is unavailable")
            .and_then(|service| {
                service
                    .submit_load_artifacts(&snapshot, generation_id, handles)
                    .map_err(|error| anyhow!("saved analysis load was rejected: {error:?}"))
            });
        match request {
            Ok(request_id) => self.pending_analysis_artifact_load = Some(request_id),
            Err(error) => {
                self.project_status_message = Some(format!(
                    "Saved analysis values could not be loaded: {error}"
                ));
            }
        }
    }

    pub(crate) fn cancel_pending_analysis_artifact_load(&mut self) {
        let Some(request_id) = self.pending_analysis_artifact_load.take() else {
            return;
        };
        if let Some(service) = self.project_store.as_mut()
            && let Err(error) = service.cancel_artifact_load(request_id)
        {
            tracing::debug!(?error, "analysis artifact load was already complete");
        }
        self.analysis_runtime.drop_authenticated_bundles();
    }

    pub(crate) fn pump_analysis_requests(&mut self) -> Result<()> {
        self.reconcile_analysis_currentness()?;
        while let Some(demand) = self.analysis_runtime.next_demand() {
            let request = demand.request();
            ensure!(
                request.priority() == RequestPriority::Analysis,
                "analysis demand did not use analysis priority"
            );
            ensure!(
                request.generation() == self.dataset.dispatcher_mut().generation(SCOPE_ANALYSIS),
                "analysis demand used a stale cancellation generation"
            );
            let ticket = match self.dataset.dispatcher_mut().submit_if_missing(
                SCOPE_ANALYSIS,
                request.resource(),
                RequestPriority::Analysis,
                false,
            ) {
                Ok(Some(ticket)) => ticket,
                Ok(None) => break,
                Err(fault) if fault.code() == RuntimeFaultCode::QueueFull => break,
                Err(fault) => return Err(anyhow!("analysis dataset request failed: {fault}")),
            };
            self.analysis_runtime.register_submission(demand, ticket)?;
        }
        Ok(())
    }

    pub(crate) fn handle_analysis_runtime_event(
        &mut self,
        token: OperationToken,
        event: CompletionEvent,
    ) {
        match event {
            CompletionEvent::Progressed(_) => {
                if let Err(error) = self.pump_analysis_requests() {
                    self.abort_running_analysis(
                        &token,
                        OperationFailureCode::AnalysisExecutionFailed,
                        &error,
                    );
                }
            }
            CompletionEvent::PendingCommitReady => {
                let result = self
                    .analysis_runtime
                    .stage_pending_commit()
                    .and_then(|stage| {
                        ensure!(stage.token == token, "analysis staging token changed");
                        current_egui_shell_bridge::dispatch(
                            &mut self.application,
                            ApplicationCommand::StageAnalysisBundle {
                                token: stage.token,
                                artifacts: stage.references,
                            },
                        )
                        .map_err(|fault| {
                            anyhow!("analysis result staging was rejected: {fault:?}")
                        })?;
                        Ok(())
                    });
                if let Err(error) = result {
                    let _ = self.analysis_runtime.drop_commit(&token);
                    self.complete_background_operation(
                        token,
                        OperationCompletion::Failed(OperationFailureCode::AnalysisExecutionFailed),
                    );
                    self.project_status_message =
                        Some(format!("Analysis could not be saved: {error}"));
                    return;
                }
                self.pump_application_services();
            }
            CompletionEvent::Cancelled => {
                self.complete_background_operation(token, OperationCompletion::Cancelled);
            }
            CompletionEvent::Failed(failure) => {
                let code = analysis_failure_code(&failure);
                self.complete_background_operation(token, OperationCompletion::Failed(code));
                self.project_status_message = Some(format!("Analysis failed: {failure}"));
                if let AnalysisFailure::Dataset(fault) = &failure
                    && crate::workbench_brick_runtime::runtime_fault_invalidates_verified_source(
                        fault.code(),
                    )
                {
                    self.record_dataset_fault(fault);
                }
            }
            CompletionEvent::IgnoredRetired => {}
        }
    }

    pub(crate) fn reconcile_analysis_currentness(&mut self) -> Result<()> {
        let Some(token) = self.analysis_runtime.active_token().cloned() else {
            return Ok(());
        };
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let token_registered = snapshot
            .active_operations()
            .iter()
            .any(|active| active == &token);
        if token_registered && token.currentness_generation() == snapshot.currentness() {
            return Ok(());
        }
        if self.analysis_runtime.commit_submitted() {
            if let Some(service) = self.project_store.as_mut()
                && let Err(error) = service.cancel_operation(token.operation_id())
            {
                tracing::warn!(?error, "stale analysis-store cancellation was rejected");
            }
        } else {
            let next = self
                .dataset
                .dispatcher_mut()
                .advance_scope(SCOPE_ANALYSIS)
                .map_err(|fault| anyhow!("analysis cancellation failed: {fault}"))?;
            self.analysis_runtime.cancel(next)?;
        }
        if token_registered {
            let _ = current_egui_shell_bridge::dispatch(
                &mut self.application,
                ApplicationCommand::CancelOperation(token.operation_id()),
            );
        }
        Ok(())
    }

    pub(crate) fn cancel_analysis_worker_for_event(&mut self, token: &OperationToken) {
        if self.analysis_runtime.active_token() != Some(token) {
            return;
        }
        if self.analysis_runtime.commit_submitted() {
            if let Some(service) = self.project_store.as_mut()
                && let Err(error) = service.cancel_operation(token.operation_id())
            {
                tracing::warn!(?error, "analysis-store cancellation was rejected");
            }
            return;
        }
        match self.dataset.dispatcher_mut().advance_scope(SCOPE_ANALYSIS) {
            Ok(next) => {
                if let Err(error) = self.analysis_runtime.cancel(next) {
                    tracing::warn!(%error, "analysis cancellation failed");
                }
            }
            Err(fault) => tracing::warn!(%fault, "analysis runtime cancellation failed"),
        }
    }

    pub(crate) fn abort_running_analysis(
        &mut self,
        token: &OperationToken,
        code: OperationFailureCode,
        error: &anyhow::Error,
    ) {
        if let Ok(next) = self.dataset.dispatcher_mut().advance_scope(SCOPE_ANALYSIS) {
            let _ = self.analysis_runtime.cancel(next);
        }
        self.complete_background_operation(token.clone(), OperationCompletion::Failed(code));
        self.project_status_message = Some(format!("Analysis failed: {error}"));
    }
}

fn analysis_failure_code(failure: &AnalysisFailure) -> OperationFailureCode {
    match failure {
        AnalysisFailure::Dataset(fault) if runtime_fault_is_capacity(fault) => {
            OperationFailureCode::AnalysisCapacityExceeded
        }
        AnalysisFailure::Dataset(_)
        | AnalysisFailure::Reduction(_)
        | AnalysisFailure::ResultReservationExceeded { .. } => {
            OperationFailureCode::AnalysisExecutionFailed
        }
    }
}

fn runtime_fault_is_capacity(fault: &RuntimeFault) -> bool {
    matches!(
        fault.code(),
        RuntimeFaultCode::CapacityExceeded { .. }
            | RuntimeFaultCode::MinimumWorkUnitExceedsBudget
            | RuntimeFaultCode::QueueFull
    )
}
