//! Product-side ownership for one bounded analysis session.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, anyhow, bail, ensure};
use mirante4d_analysis_core::{
    AnalysisDefinition, AnalysisOperation, AnalysisPlot, AnalysisPlotArtifact, AnalysisTable,
    AnalysisTableArtifact,
};
use mirante4d_analysis_runtime::{
    AnalysisDemand, AnalysisProgress, AnalysisRuntime, CancelEvent, CompletionEvent,
    PendingAnalysisCommit,
};
use mirante4d_application::{
    AnalysisPlotDescriptor, AnalysisPlotId, AnalysisTableDescriptor, AnalysisTableId,
    LoadedAnalysisDescriptorBundle, OperationKind, OperationToken,
};
use mirante4d_dataset_runtime::{
    AccountedCpuLease, CancellationGeneration, RequestTicket, RuntimeOutcome,
};
use mirante4d_project_model::{
    ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
    ArtifactSchema, DatasetReference,
};
use mirante4d_project_store::{LoadedProjectArtifact, ProjectObjectBytes, ProjectObjectSource};
use uuid::Uuid;

const READY_STATUS: &str = "Analysis is ready.";

/// Stable project facts prepared before the application stages an analysis
/// bundle. Object sources remain in the runtime until the application emits
/// the matching project-store request.
#[derive(Debug, Clone)]
pub(crate) struct AnalysisCommitStage {
    pub(crate) token: OperationToken,
    pub(crate) references: Vec<ArtifactReference>,
}

/// One product analysis session. Dataset polling and project persistence stay
/// with their existing owners; this value only bridges their typed results.
#[derive(Debug)]
pub(crate) struct AnalysisProductRuntime {
    execution: AnalysisRuntime,
    active_token: Option<OperationToken>,
    pending: Option<ProductPendingCommit>,
    tables: BTreeMap<AnalysisTableId, Arc<AnalysisTable>>,
    plots: BTreeMap<AnalysisPlotId, Arc<AnalysisPlot>>,
    pending_load: Option<Vec<DecodedBundle>>,
    progress: Option<AnalysisProgress>,
    status_text: String,
    roi_origin: [u64; 3],
    roi_shape: [u64; 3],
    last_export_csv: Option<String>,
}

#[derive(Debug)]
struct ProductPendingCommit {
    result: PendingAnalysisCommit,
    references: Vec<ArtifactReference>,
    table: AnalysisTableDescriptor,
    plot: Option<AnalysisPlotDescriptor>,
    object_sources: Option<Vec<ProjectObjectBytes>>,
    sources_taken: bool,
}

#[derive(Debug)]
struct DecodedBundle {
    artifacts: Vec<ArtifactReference>,
    table_id: AnalysisTableId,
    table: AnalysisTable,
    table_descriptor: AnalysisTableDescriptor,
    plot: Option<(AnalysisPlotId, AnalysisPlot)>,
    plot_descriptor: Option<AnalysisPlotDescriptor>,
}

impl Default for AnalysisProductRuntime {
    fn default() -> Self {
        Self {
            execution: AnalysisRuntime::new(),
            active_token: None,
            pending: None,
            tables: BTreeMap::new(),
            plots: BTreeMap::new(),
            pending_load: None,
            progress: None,
            status_text: READY_STATUS.to_owned(),
            roi_origin: [0; 3],
            roi_shape: [1; 3],
            last_export_csv: None,
        }
    }
}

impl AnalysisProductRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn active_token(&self) -> Option<&OperationToken> {
        self.active_token.as_ref()
    }

    pub(crate) fn status_text(&self) -> &str {
        &self.status_text
    }

    pub(crate) const fn progress(&self) -> Option<AnalysisProgress> {
        self.progress
    }

    pub(crate) fn commit_submitted(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.sources_taken)
    }

    pub(crate) const fn roi_origin(&self) -> [u64; 3] {
        self.roi_origin
    }

    pub(crate) const fn roi_shape(&self) -> [u64; 3] {
        self.roi_shape
    }

    pub(crate) fn set_roi(&mut self, origin: [u64; 3], shape: [u64; 3]) -> Result<()> {
        ensure!(
            shape.into_iter().all(|dimension| dimension != 0),
            "analysis ROI dimensions must be nonzero"
        );
        self.roi_origin = origin;
        self.roi_shape = shape;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn table(&self, id: AnalysisTableId) -> Option<&AnalysisTable> {
        self.tables.get(&id).map(Arc::as_ref)
    }

    #[cfg(test)]
    pub(crate) fn plot(&self, id: AnalysisPlotId) -> Option<&AnalysisPlot> {
        self.plots.get(&id).map(Arc::as_ref)
    }

    pub(crate) fn table_handle(&self, id: AnalysisTableId) -> Option<Arc<AnalysisTable>> {
        self.tables.get(&id).cloned()
    }

    pub(crate) fn plot_handle(&self, id: AnalysisPlotId) -> Option<Arc<AnalysisPlot>> {
        self.plots.get(&id).cloned()
    }

    pub(crate) fn start(
        &mut self,
        token: OperationToken,
        definition: AnalysisDefinition,
        generation: CancellationGeneration,
        result_charge: AccountedCpuLease,
    ) -> Result<()> {
        ensure!(self.active_token.is_none(), "an analysis is already active");
        ensure!(
            self.pending.is_none(),
            "an analysis commit is already pending"
        );
        ensure!(
            self.pending_load.is_none(),
            "persisted analysis results are still being installed"
        );
        ensure!(
            token.kind() == OperationKind::Analysis,
            "the operation token is not for analysis"
        );
        let revision = token
            .project_revision()
            .context("analysis requires a bound project revision")?;
        ensure!(
            token.project_id() == Some(revision.project_id()),
            "analysis token project facts disagree"
        );
        ensure!(
            token.source_identity() == Some(definition.source_content_id()),
            "analysis token and definition use different scientific sources"
        );

        self.execution
            .start(definition, revision, generation, result_charge)
            .context("could not start analysis execution")?;
        self.active_token = Some(token);
        self.progress = self.execution.progress();
        self.refresh_running_status();
        Ok(())
    }

    pub(crate) fn next_demand(&self) -> Option<AnalysisDemand> {
        self.execution.next_demand()
    }

    pub(crate) fn register_submission(
        &mut self,
        demand: AnalysisDemand,
        ticket: RequestTicket,
    ) -> Result<()> {
        self.execution
            .register_submission(demand, ticket)
            .context("could not register the analysis request")?;
        self.progress = self.execution.progress();
        self.refresh_running_status();
        Ok(())
    }

    pub(crate) fn accept_completion(
        &mut self,
        ticket: RequestTicket,
        outcome: RuntimeOutcome,
    ) -> Result<CompletionEvent> {
        let event = self
            .execution
            .accept_completion(ticket, outcome)
            .context("could not accept the analysis completion")?;
        match &event {
            CompletionEvent::Progressed(progress) => {
                self.progress = Some(*progress);
                self.refresh_running_status();
            }
            CompletionEvent::PendingCommitReady => {
                let pending = self
                    .execution
                    .take_pending_commit()
                    .context("analysis completed without a pending artifact bundle")?;
                match ProductPendingCommit::new(pending) {
                    Ok(pending) => {
                        self.pending = Some(pending);
                        self.progress = None;
                        self.status_text = "Analysis complete; preparing project save.".to_owned();
                    }
                    Err(error) => {
                        self.active_token = None;
                        self.progress = None;
                        self.status_text = "Analysis result could not be prepared.".to_owned();
                        return Err(error);
                    }
                }
            }
            CompletionEvent::Cancelled => {
                self.active_token = None;
                self.progress = None;
                self.status_text = "Analysis cancelled.".to_owned();
            }
            CompletionEvent::Failed(failure) => {
                self.active_token = None;
                self.progress = None;
                self.status_text = format!("Analysis failed: {failure}");
            }
            CompletionEvent::IgnoredRetired => {}
        }
        Ok(event)
    }

    pub(crate) fn cancel(
        &mut self,
        next_generation: CancellationGeneration,
    ) -> Result<CancelEvent> {
        let event = if let Some(pending) = &self.pending {
            ensure!(
                pending.result.generation().checked_next()? == next_generation,
                "analysis cancellation must advance exactly one generation"
            );
            self.pending = None;
            CancelEvent::DiscardedPendingCommit
        } else {
            self.execution
                .cancel(next_generation)
                .context("could not cancel analysis execution")?
        };
        if event != CancelEvent::Idle {
            self.active_token = None;
            self.progress = None;
            self.status_text = "Analysis cancelled.".to_owned();
        }
        Ok(event)
    }

    /// Creates stable references and exact-byte object sources once. Repeated
    /// staging would otherwise risk sending the same operation twice.
    pub(crate) fn stage_pending_commit(&mut self) -> Result<AnalysisCommitStage> {
        let token = self
            .active_token
            .as_ref()
            .context("analysis has no active operation token")?
            .clone();
        let pending = self
            .pending
            .as_mut()
            .context("analysis has no pending artifact bundle")?;
        ensure!(
            pending.object_sources.is_none() && !pending.sources_taken,
            "analysis artifact bundle was already staged"
        );
        pending.object_sources = Some(pending.build_object_sources()?);
        self.status_text = "Saving analysis results…".to_owned();
        Ok(AnalysisCommitStage {
            token,
            references: pending.references.clone(),
        })
    }

    /// Transfers the already-validated exact-byte sources to the existing
    /// project-store service after the application supplies its projection.
    pub(crate) fn take_staged_object_sources(
        &mut self,
        token: &OperationToken,
    ) -> Result<Vec<Box<dyn ProjectObjectSource>>> {
        self.validate_active_token(token)?;
        let pending = self
            .pending
            .as_mut()
            .context("analysis has no pending artifact bundle")?;
        let sources = pending
            .object_sources
            .take()
            .context("analysis artifact sources were not staged or were already taken")?;
        pending.sources_taken = true;
        Ok(sources
            .into_iter()
            .map(|source| Box::new(source) as Box<dyn ProjectObjectSource>)
            .collect())
    }

    /// Returns descriptor clones for reducer admission without exposing the
    /// decoded values or releasing their accounting charge.
    pub(crate) fn staged_descriptors(
        &self,
        token: &OperationToken,
    ) -> Result<(AnalysisTableDescriptor, Option<AnalysisPlotDescriptor>)> {
        self.validate_active_token(token)?;
        let pending = self
            .pending
            .as_ref()
            .context("analysis has no pending artifact bundle")?;
        ensure!(
            pending.object_sources.is_some() || pending.sources_taken,
            "analysis artifact bundle was not staged"
        );
        ensure!(
            !self.tables.contains_key(&pending.table.id()),
            "analysis table handle already exists"
        );
        if let Some(plot) = &pending.plot {
            ensure!(
                !self.plots.contains_key(&plot.id()),
                "analysis plot handle already exists"
            );
        }
        Ok((pending.table.clone(), pending.plot.clone()))
    }

    /// Installs both decoded values only after the project store reports the
    /// atomic bundle commit successful, then releases the result-byte charge.
    pub(crate) fn finish_commit(&mut self, token: &OperationToken) -> Result<()> {
        self.validate_active_token(token)?;
        let pending = self
            .pending
            .as_ref()
            .context("analysis has no pending artifact bundle")?;
        ensure!(
            pending.sources_taken,
            "analysis object sources were not submitted"
        );
        let table_id = pending.table.id();
        let plot_id = pending.plot.as_ref().map(AnalysisPlotDescriptor::id);
        ensure!(
            !self.tables.contains_key(&table_id),
            "analysis table handle already exists"
        );
        if let Some(plot_id) = plot_id {
            ensure!(
                !self.plots.contains_key(&plot_id),
                "analysis plot handle already exists"
            );
        }

        let pending = self
            .pending
            .take()
            .expect("pending commit was checked above");
        let table = pending.result.artifacts().table().value().clone();
        let plot = pending
            .result
            .artifacts()
            .plot()
            .map(|artifact| artifact.value().clone());
        self.tables.insert(table_id, Arc::new(table));
        if let Some((plot_id, plot)) = plot_id.zip(plot) {
            self.plots.insert(plot_id, Arc::new(plot));
        }
        self.active_token = None;
        self.progress = None;
        self.status_text = "Analysis complete.".to_owned();
        Ok(())
    }

    /// Drops a pending result after staging or project-store failure.
    pub(crate) fn drop_commit(&mut self, token: &OperationToken) -> Result<()> {
        self.validate_active_token(token)?;
        ensure!(self.pending.is_some(), "analysis has no pending commit");
        self.pending = None;
        self.active_token = None;
        self.progress = None;
        self.status_text = "Analysis results were not saved.".to_owned();
        Ok(())
    }

    /// Decodes, authenticates, and cross-checks one persisted table/plot
    /// bundle before either value becomes visible in memory.
    pub(crate) fn stage_authenticated_bundles(
        &mut self,
        artifacts: Vec<LoadedProjectArtifact>,
        expected_source: &DatasetReference,
    ) -> Result<Vec<LoadedAnalysisDescriptorBundle>> {
        ensure!(
            self.active_token.is_none(),
            "cannot install persisted analysis while another analysis is active"
        );
        ensure!(
            self.pending_load.is_none(),
            "persisted analysis results are already staged"
        );
        ensure!(!artifacts.is_empty(), "persisted analysis bundle is empty");
        let mut grouped = BTreeMap::<[u8; 15], Vec<LoadedProjectArtifact>>::new();
        for artifact in artifacts {
            let key = persisted_bundle_key(artifact.reference())?;
            grouped.entry(key).or_default().push(artifact);
        }
        let bundles = grouped
            .into_values()
            .map(decode_loaded_bundle)
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            bundles
                .iter()
                .all(|bundle| bundle.table.provenance().source_content_id()
                    == *expected_source.scientific_content_id()),
            "persisted analysis was derived from a different microscopy source"
        );

        let mut table_ids = self
            .tables
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let mut plot_ids = self
            .plots
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for bundle in &bundles {
            ensure!(
                table_ids.insert(bundle.table_id),
                "persisted analysis table handle is duplicated"
            );
            if let Some((plot_id, _)) = &bundle.plot {
                ensure!(
                    plot_ids.insert(*plot_id),
                    "persisted analysis plot handle is duplicated"
                );
            }
        }

        let descriptors = bundles
            .iter()
            .map(|bundle| {
                LoadedAnalysisDescriptorBundle::new(
                    bundle.artifacts.clone(),
                    bundle.table_descriptor.clone(),
                    bundle.plot_descriptor.clone(),
                )
            })
            .collect();
        self.pending_load = Some(bundles);
        Ok(descriptors)
    }

    pub(crate) fn finish_authenticated_bundles(&mut self) -> Result<()> {
        let bundles = self
            .pending_load
            .take()
            .context("persisted analysis results were not staged")?;
        for bundle in bundles {
            self.tables.insert(bundle.table_id, Arc::new(bundle.table));
            if let Some((plot_id, plot)) = bundle.plot {
                self.plots.insert(plot_id, Arc::new(plot));
            }
        }
        self.status_text = "Loaded saved analysis results.".to_owned();
        Ok(())
    }

    pub(crate) fn drop_authenticated_bundles(&mut self) {
        self.pending_load = None;
    }

    pub(crate) fn export_selected_table_csv(
        &mut self,
        selected: Option<AnalysisTableId>,
    ) -> Result<&str> {
        let id = selected.context("no analysis table is selected")?;
        let csv = self
            .tables
            .get(&id)
            .context("selected analysis table is not loaded")?
            .to_csv();
        self.last_export_csv = Some(csv);
        Ok(self
            .last_export_csv
            .as_deref()
            .expect("the export was assigned above"))
    }

    pub(crate) fn last_export_csv(&self) -> Option<&str> {
        self.last_export_csv.as_deref()
    }

    pub(crate) fn clear_loaded(&mut self) {
        self.pending_load = None;
        self.tables.clear();
        self.plots.clear();
        self.last_export_csv = None;
        if self.active_token.is_none() {
            self.status_text = READY_STATUS.to_owned();
        }
    }

    fn refresh_running_status(&mut self) {
        if let Some(progress) = self.progress {
            self.status_text = format!(
                "Analyzing: {} of {} blocks complete.",
                progress.completed_blocks(),
                progress.total_blocks()
            );
        }
    }

    fn validate_active_token(&self, token: &OperationToken) -> Result<()> {
        ensure!(
            self.active_token.as_ref() == Some(token),
            "analysis operation token does not match"
        );
        Ok(())
    }
}

impl ProductPendingCommit {
    fn new(result: PendingAnalysisCommit) -> Result<Self> {
        let artifacts = result.artifacts();
        validate_table_plot_pair(
            artifacts.table().value(),
            artifacts.plot().map(AnalysisPlotArtifact::value),
        )?;

        let (table_handle, plot_handle) = fresh_bundle_handles(artifacts.plot().is_some());
        let table_reference = table_reference(artifacts.table(), table_handle.clone())?;
        let table = table_descriptor(artifacts.table(), &table_handle)?;
        let (plot_reference, plot) = if let Some(artifact) = artifacts.plot() {
            let handle = plot_handle.expect("a plot bundle receives a plot handle");
            (
                Some(plot_reference(artifact, handle.clone())?),
                Some(plot_descriptor(artifact, &handle)?),
            )
        } else {
            (None, None)
        };
        let mut references = vec![table_reference];
        references.extend(plot_reference);
        Ok(Self {
            result,
            references,
            table,
            plot,
            object_sources: None,
            sources_taken: false,
        })
    }

    fn build_object_sources(&self) -> Result<Vec<ProjectObjectBytes>> {
        let artifacts = self.result.artifacts();
        let mut sources = Vec::with_capacity(usize::from(artifacts.plot().is_some()) + 1);
        sources.push(
            ProjectObjectBytes::new(
                artifacts.table().descriptor().clone(),
                artifacts.table().bytes().to_vec(),
            )
            .context("analysis table bytes did not match their descriptor")?,
        );
        if let Some(plot) = artifacts.plot() {
            sources.push(
                ProjectObjectBytes::new(plot.descriptor().clone(), plot.bytes().to_vec())
                    .context("analysis plot bytes did not match their descriptor")?,
            );
        }
        Ok(sources)
    }
}

fn fresh_bundle_handles(has_plot: bool) -> (ArtifactHandleId, Option<ArtifactHandleId>) {
    // The shared random prefix is the durable bundle identity; the final byte
    // distinguishes its table and plot so exact reruns remain pairable.
    let mut table = *Uuid::new_v4().as_bytes();
    table[15] = 0;
    let plot = has_plot.then(|| {
        let mut plot = table;
        plot[15] = 1;
        ArtifactHandleId::from_bytes(plot)
    });
    (ArtifactHandleId::from_bytes(table), plot)
}

fn persisted_bundle_key(reference: &ArtifactReference) -> Result<[u8; 15]> {
    ensure!(
        reference.derivation_id().is_some(),
        "persisted analysis artifact has no derivation identity"
    );
    let bytes = reference.handle_id().bytes();
    let expected_role = match reference.schema() {
        ArtifactSchema::AnalysisTableV1 => 0,
        ArtifactSchema::AnalysisPlotV1 => 1,
        _ => bail!("persisted bundle contains a non-analysis artifact"),
    };
    ensure!(
        bytes[15] == expected_role,
        "persisted analysis handle does not encode its bundle role"
    );
    Ok(bytes[..15]
        .try_into()
        .expect("a 16-byte handle always has a 15-byte bundle prefix"))
}

fn table_reference(
    artifact: &AnalysisTableArtifact,
    handle: ArtifactHandleId,
) -> Result<ArtifactReference> {
    let provenance = artifact.value().provenance();
    ArtifactReference::new(
        handle,
        ArtifactSchema::AnalysisTableV1,
        artifact.content_id(),
        artifact.descriptor().clone(),
        Some(provenance.derivation_id()),
        Some(provenance.recipe_id()),
        vec![provenance.source_layer()],
        artifact.value().name(),
        true,
        ArtifactCompleteness::Complete,
        ArtifactRecoverability::Regenerable,
    )
    .context("could not construct the analysis table reference")
}

fn plot_reference(
    artifact: &AnalysisPlotArtifact,
    handle: ArtifactHandleId,
) -> Result<ArtifactReference> {
    let provenance = artifact.value().provenance();
    ArtifactReference::new(
        handle,
        ArtifactSchema::AnalysisPlotV1,
        artifact.content_id(),
        artifact.descriptor().clone(),
        Some(provenance.derivation_id()),
        Some(provenance.recipe_id()),
        vec![provenance.source_layer()],
        artifact.value().name(),
        true,
        ArtifactCompleteness::Complete,
        ArtifactRecoverability::Regenerable,
    )
    .context("could not construct the analysis plot reference")
}

fn table_descriptor(
    artifact: &AnalysisTableArtifact,
    handle: &ArtifactHandleId,
) -> Result<AnalysisTableDescriptor> {
    let row_count = u64::try_from(artifact.value().rows().len())
        .context("analysis table row count exceeds u64")?;
    Ok(AnalysisTableDescriptor::new(
        AnalysisTableId::from_artifact_handle(handle),
        row_count,
    ))
}

fn plot_descriptor(
    artifact: &AnalysisPlotArtifact,
    handle: &ArtifactHandleId,
) -> Result<AnalysisPlotDescriptor> {
    let point_count = u64::try_from(artifact.value().points().len())
        .context("analysis plot point count exceeds u64")?;
    AnalysisPlotDescriptor::new(
        AnalysisPlotId::from_artifact_handle(handle),
        vec![point_count],
    )
    .map_err(|error| anyhow!("invalid analysis plot descriptor: {error:?}"))
}

fn decode_loaded_bundle(artifacts: Vec<LoadedProjectArtifact>) -> Result<DecodedBundle> {
    ensure!(
        matches!(artifacts.len(), 1 | 2),
        "persisted analysis bundle must contain one table and at most one plot"
    );
    let mut table = None::<(ArtifactHandleId, AnalysisTableArtifact)>;
    let mut plot = None::<(ArtifactHandleId, AnalysisPlotArtifact)>;
    let mut references = Vec::with_capacity(artifacts.len());
    for loaded in artifacts {
        let (reference, bytes) = loaded.into_parts();
        references.push(reference.clone());
        let handle = reference.handle_id().clone();
        match reference.schema() {
            ArtifactSchema::AnalysisTableV1 => {
                ensure!(table.is_none(), "analysis bundle contains two tables");
                let decoded = AnalysisTableArtifact::decode(&bytes)
                    .context("persisted analysis table is invalid")?;
                ensure!(
                    table_reference(&decoded, handle.clone())? == reference,
                    "persisted analysis table reference does not match its canonical bytes"
                );
                table = Some((handle, decoded));
            }
            ArtifactSchema::AnalysisPlotV1 => {
                ensure!(plot.is_none(), "analysis bundle contains two plots");
                let decoded = AnalysisPlotArtifact::decode(&bytes)
                    .context("persisted analysis plot is invalid")?;
                ensure!(
                    plot_reference(&decoded, handle.clone())? == reference,
                    "persisted analysis plot reference does not match its canonical bytes"
                );
                plot = Some((handle, decoded));
            }
            _ => bail!("persisted bundle contains a non-analysis artifact"),
        }
    }
    let (table_handle, table_artifact) = table.context("analysis bundle has no table")?;
    ensure!(
        plot.as_ref()
            .is_none_or(|(plot_handle, _)| plot_handle != &table_handle),
        "analysis table and plot share one durable handle"
    );
    validate_table_plot_pair(
        table_artifact.value(),
        plot.as_ref().map(|(_, artifact)| artifact.value()),
    )?;

    let table_descriptor = table_descriptor(&table_artifact, &table_handle)?;
    let plot_descriptor = plot
        .as_ref()
        .map(|(handle, artifact)| plot_descriptor(artifact, handle))
        .transpose()?;
    Ok(DecodedBundle {
        artifacts: references,
        table_id: table_descriptor.id(),
        table: table_artifact.value().clone(),
        table_descriptor,
        plot: plot.map(|(handle, artifact)| {
            (
                AnalysisPlotId::from_artifact_handle(&handle),
                artifact.value().clone(),
            )
        }),
        plot_descriptor,
    })
}

fn validate_table_plot_pair(table: &AnalysisTable, plot: Option<&AnalysisPlot>) -> Result<()> {
    match table.provenance().operation() {
        AnalysisOperation::FullIntensitySummary => {
            ensure!(plot.is_some(), "full summary is missing its plot");
        }
        AnalysisOperation::BoxRoiIntensityStatistics => {
            ensure!(
                plot.is_none(),
                "box ROI result unexpectedly contains a plot"
            );
        }
    }
    let Some(plot) = plot else {
        return Ok(());
    };
    ensure!(
        plot.provenance() == table.provenance(),
        "analysis table and plot provenance disagree"
    );
    ensure!(
        plot.points().len() == table.rows().len()
            && plot.points().iter().zip(table.rows()).all(|(point, row)| {
                point.timepoint() == row.timepoint() && point.mean() == row.mean()
            }),
        "analysis plot does not represent its table"
    );
    Ok(())
}
