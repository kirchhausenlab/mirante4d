//! Generation-last publication and project-lane authority updates.
//!
//! This private B2 slice installs a complete initial package and advances
//! established manual and autosave heads through recovery-before-head atomic
//! replacement under a held writer lease and bounded whole-store inventory. It
//! also installs and advances the private provisional autosave-only lane.
//! Public actor execution for Create and Save As, recovery selection, timers,
//! and garbage collection remain outside this module.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{BTreeMap, btree_map::Entry},
    io::{self, Cursor, Read},
};

use mirante4d_identity::{ExactBytesDigest, ExactBytesHasher, RawObjectDescriptor};
use thiserror::Error;

use crate::{
    ProjectCommitCapture, ProjectGenerationId, ProjectStoreFault, ProjectStoreLimits,
    ProjectStorePath, ProjectStoreReceipt,
    api::CapturedObjectSource,
    generation::{
        ArtifactStorage, EncodedGeneration, GenerationCodecError, GenerationDocument,
        GenerationKind, LogicalObjectBinding, LogicalObjectPage, PAGE_BYTES, PhysicalObject,
    },
    inspection::{
        EstablishedStoreInspection, LaneSnapshot, inspect_established_store, inspect_store_graph,
        inspect_store_state, read_lane_snapshot,
    },
    lease::{LeaseError, ProjectStoreLeases},
    local::{
        ImmutablePublication, LocalPublicationError, LocalStoreRoot, PublicationDisposition,
        SiblingPackageStage,
    },
    wire::{ProjectEnvelope, RefKind, RefRecord},
};

const STREAM_BUFFER_BYTES_MAX: usize = 1_048_576;
const PAGE_RECORDS_MAX: u64 = 65_536;

#[derive(Debug, Error)]
pub(crate) enum GenerationTransactionError {
    #[error("generation publication was cancelled before any authority changed")]
    Cancelled,
    #[error("a captured logical object changed while it was streamed")]
    SourceChanged,
    #[error("a captured logical object source could not be opened or read")]
    SourceIo,
    #[error("generation publication exceeded the bound for {stage}")]
    Capacity { stage: &'static str },
    #[error("the physical object closure is invalid")]
    Closure,
    #[error(transparent)]
    Codec(#[from] GenerationCodecError),
    #[error(transparent)]
    Local(LocalPublicationError),
}

impl From<LocalPublicationError> for GenerationTransactionError {
    fn from(error: LocalPublicationError) -> Self {
        match error {
            LocalPublicationError::Cancelled => Self::Cancelled,
            LocalPublicationError::SourceLength { .. } | LocalPublicationError::SourceDigest => {
                Self::SourceChanged
            }
            LocalPublicationError::Capacity { .. } => Self::Capacity {
                stage: "physical object",
            },
            other => Self::Local(other),
        }
    }
}

#[derive(Debug)]
pub(crate) struct UnreferencedGenerationPublication {
    document: GenerationDocument,
    generation: EncodedGeneration,
    generation_disposition: PublicationDisposition,
    created_objects: u64,
    created_object_bytes: u64,
}

impl UnreferencedGenerationPublication {
    pub(crate) const fn generation_id(&self) -> ProjectGenerationId {
        self.generation.id()
    }

    pub(crate) fn generation_bytes(&self) -> &[u8] {
        self.generation.bytes()
    }

    pub(crate) fn document(&self) -> &GenerationDocument {
        &self.document
    }

    pub(crate) const fn generation_disposition(&self) -> PublicationDisposition {
        self.generation_disposition
    }

    pub(crate) const fn created_objects(&self) -> u64 {
        self.created_objects
    }

    pub(crate) const fn created_object_bytes(&self) -> u64 {
        self.created_object_bytes
    }
}

/// Why a new package receives its first manual generation.
///
/// Keeping this private prevents callers from installing arbitrary fork
/// provenance while Create and Save As are still off-product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitialPackageMode {
    Create,
    SaveAs {
        source_project_id: mirante4d_project_model::ProjectId,
        source_generation_id: ProjectGenerationId,
    },
}

impl InitialPackageMode {
    const fn expected_fork(
        self,
    ) -> Option<(mirante4d_project_model::ProjectId, ProjectGenerationId)> {
        match self {
            Self::Create => None,
            Self::SaveAs {
                source_project_id,
                source_generation_id,
            } => Some((source_project_id, source_generation_id)),
        }
    }
}

/// A durably installed package together with the exact descriptor-held
/// resources that crossed the no-replace rename.
pub(crate) struct InstalledInitialPackage {
    root: LocalStoreRoot,
    leases: ProjectStoreLeases,
    receipt: ProjectStoreReceipt,
}

impl InstalledInitialPackage {
    pub(crate) fn receipt(&self) -> &ProjectStoreReceipt {
        &self.receipt
    }

    pub(crate) fn into_parts(self) -> (LocalStoreRoot, ProjectStoreLeases, ProjectStoreReceipt) {
        (self.root, self.leases, self.receipt)
    }
}

/// Builds one complete private sibling package and installs it exactly once.
/// No destination path becomes authoritative before the final no-replace
/// rename, and the source project is represented only by immutable provenance
/// plus read-only object streams.
pub(crate) fn install_initial_manual_package<C>(
    destination: &ProjectStorePath,
    mode: InitialPackageMode,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<InstalledInitialPackage, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let (stage, leases, receipt) =
        prepare_initial_manual_package(destination, mode, capture, limits, &mut is_cancelled)?;
    let root = stage
        .install(&mut is_cancelled)
        .map_err(|error| map_local_error(error, "package_install"))?;
    Ok(InstalledInitialPackage {
        root,
        leases,
        receipt,
    })
}

/// Installs the first private autosave-only package at the caller's exact
/// destination. A visible destination is never overwritten: it may be adopted
/// only when a bounded stable full verification proves that it is the exact
/// package produced by this first capture.
pub(crate) fn install_initial_provisional_autosave_package<C>(
    destination: &ProjectStorePath,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<InstalledInitialPackage, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    install_initial_provisional_autosave_package_inner(
        destination,
        capture,
        limits,
        &mut is_cancelled,
        false,
    )
}

#[cfg(test)]
fn install_initial_provisional_autosave_package_with_parent_sync_failure<C>(
    destination: &ProjectStorePath,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<InstalledInitialPackage, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    install_initial_provisional_autosave_package_inner(
        destination,
        capture,
        limits,
        &mut is_cancelled,
        true,
    )
}

fn install_initial_provisional_autosave_package_inner<C>(
    destination: &ProjectStorePath,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    fail_parent_sync: bool,
) -> Result<InstalledInitialPackage, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    let project_id = validate_initial_provisional_capture(&capture)?;
    check_cancelled(&mut *is_cancelled).map_err(map_generation_error)?;
    let stage = match SiblingPackageStage::begin(destination, limits) {
        Ok(stage) => stage,
        Err(LocalPublicationError::DestinationExists) => {
            return adopt_initial_provisional_autosave_package(
                destination,
                &capture,
                limits,
                is_cancelled,
            );
        }
        Err(error) => return Err(map_local_error(error, "package_stage_create")),
    };
    stage
        .root()
        .publish_project_envelope(ProjectEnvelope::new(project_id), limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "package_envelope"))?;
    let leases = ProjectStoreLeases::acquire(stage.root(), crate::ProjectOpenMode::PreferWritable)
        .map_err(map_lease_error)?;
    if !leases.has_writer() {
        return Err(ProjectStoreFault::WriterContended);
    }
    let receipt = publish_initial_provisional_autosave_generation(
        stage.root(),
        &leases,
        capture,
        limits,
        &mut *is_cancelled,
    )?;
    let state = inspect_store_state(stage.root(), limits, &mut *is_cancelled)?;
    if !state.is_provisional()
        || state.project_id() != project_id
        || state.autosave().map(|lane| lane.head.current()) != Some(receipt.current_generation_id())
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "installed_provisional_validation",
        });
    }
    stage
        .sync_tree(limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "package_tree_sync"))?;
    #[cfg(test)]
    let installed = if fail_parent_sync {
        stage.install_with_parent_sync_failure(&mut *is_cancelled)
    } else {
        stage.install(&mut *is_cancelled)
    };
    #[cfg(not(test))]
    let installed = {
        debug_assert!(!fail_parent_sync);
        stage.install(&mut *is_cancelled)
    };
    let root = match installed {
        Ok(root) => root,
        Err(error) => {
            if matches!(error, LocalPublicationError::PackageCommitIndeterminate) {
                leases.suspend_writes();
            }
            return Err(map_local_error(error, "package_install"));
        }
    };
    Ok(InstalledInitialPackage {
        root,
        leases,
        receipt,
    })
}

fn adopt_initial_provisional_autosave_package<C>(
    destination: &ProjectStorePath,
    capture: &ProjectCommitCapture,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<InstalledInitialPackage, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    check_cancelled(&mut *is_cancelled).map_err(map_generation_error)?;
    let root = LocalStoreRoot::open(destination.as_path())
        .map_err(|_| ProjectStoreFault::DestinationExists)?;
    let leases = ProjectStoreLeases::acquire(&root, crate::ProjectOpenMode::PreferWritable)
        .map_err(map_lease_error)?;
    if !leases.has_writer() {
        return Err(ProjectStoreFault::WriterContended);
    }
    root.validate_store_inventory(limits, 0, false, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    root.validate_initial_package_namespace(limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "initial_package_namespace"))?;
    let graph = inspect_store_graph(&root, limits, &mut *is_cancelled)?;
    let snapshot = graph.snapshot();
    let state = graph.state();
    let Some(lane) = state.autosave() else {
        return Err(ProjectStoreFault::DestinationExists);
    };
    let Some(document) = state.autosave_generation() else {
        return Err(ProjectStoreFault::DestinationExists);
    };
    let generation_id = lane.head.current();
    let exact_first_capture = state.is_provisional()
        && state.project_id() == capture.projection().state().project_id()
        && lane.head.previous().is_none()
        && lane.head.base().is_none()
        && lane.recovery.is_none()
        && document.kind() == GenerationKind::Autosave
        && document.generation_sequence() == 0
        && document.parent_generation_id().is_none()
        && document.base_manual_generation_id().is_none()
        && document.forked_from().is_none()
        && document.projection() == capture.projection()
        && graph.pin_count() == 0
        && graph.generation_ids() == [generation_id]
        && graph.root_generation_ids() == [generation_id]
        && graph.orphan_generation_ids().is_empty()
        && graph.unrooted_object_facts().is_empty()
        && graph.object_facts() == document.reachable_objects();
    if !exact_first_capture {
        return Err(ProjectStoreFault::DestinationExists);
    }
    crate::full_verify::full_verify(&root, limits, &mut *is_cancelled)?;
    let after = inspect_store_graph(&root, limits, &mut *is_cancelled)?;
    if after.snapshot() != snapshot {
        return Err(ProjectStoreFault::SourceChanged);
    }
    if !leases.confirm_writer(&root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    check_cancelled(&mut *is_cancelled).map_err(map_generation_error)?;
    if let Err(error) = root.sync_existing_package_parent(destination, limits) {
        if matches!(error, LocalPublicationError::PackageCommitIndeterminate) {
            leases.suspend_writes();
        }
        return Err(map_local_error(error, "destination_parent_sync"));
    }
    let receipt = ProjectStoreReceipt::autosave(
        capture.projection().revision(),
        capture.projection().revision_high_water().clone(),
        generation_id,
        None,
        None,
        0,
        0,
    );
    Ok(InstalledInitialPackage {
        root,
        leases,
        receipt,
    })
}

#[cfg(test)]
fn install_initial_manual_package_with_parent_sync_failure<C>(
    destination: &ProjectStorePath,
    mode: InitialPackageMode,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<InstalledInitialPackage, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let (stage, leases, receipt) =
        prepare_initial_manual_package(destination, mode, capture, limits, &mut is_cancelled)?;
    let root = stage
        .install_with_parent_sync_failure(&mut is_cancelled)
        .map_err(|error| map_local_error(error, "package_install"))?;
    Ok(InstalledInitialPackage {
        root,
        leases,
        receipt,
    })
}

fn prepare_initial_manual_package<C>(
    destination: &ProjectStorePath,
    mode: InitialPackageMode,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<(SiblingPackageStage, ProjectStoreLeases, ProjectStoreReceipt), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    let project_id = validate_initial_capture(mode, &capture)?;
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;

    let stage = SiblingPackageStage::begin(destination, limits)
        .map_err(|error| map_local_error(error, "package_stage_create"))?;
    stage
        .root()
        .publish_project_envelope(ProjectEnvelope::new(project_id), limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "package_envelope"))?;
    let leases = ProjectStoreLeases::acquire(stage.root(), crate::ProjectOpenMode::PreferWritable)
        .map_err(map_lease_error)?;
    if !leases.has_writer() {
        return Err(ProjectStoreFault::WriterContended);
    }
    let receipt = publish_initial_manual_generation(
        stage.root(),
        &leases,
        capture,
        mode,
        limits,
        &mut is_cancelled,
    )?;
    let inspection = inspect_established_store(stage.root(), limits, &mut is_cancelled)?;
    if inspection.project_id() != project_id
        || inspection.manual().head.current() != receipt.current_generation_id()
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "installed_package_validation",
        });
    }
    stage
        .sync_tree(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "package_tree_sync"))?;
    Ok((stage, leases, receipt))
}

fn validate_initial_capture(
    mode: InitialPackageMode,
    capture: &ProjectCommitCapture,
) -> Result<mirante4d_project_model::ProjectId, ProjectStoreFault> {
    if capture.expected_parent().is_some() {
        return Err(ProjectStoreFault::StaleParent);
    }
    if capture.autosave_base().is_some() || capture.forked_from() != mode.expected_fork() {
        return Err(ProjectStoreFault::Corruption {
            stage: "initial_manual_capture",
        });
    }
    let project_id = capture.projection().state().project_id();
    if matches!(
        mode,
        InitialPackageMode::SaveAs {
            source_project_id,
            ..
        } if source_project_id == project_id
    ) {
        return Err(ProjectStoreFault::Corruption {
            stage: "save_as_project_identity",
        });
    }
    Ok(project_id)
}

fn validate_initial_provisional_capture(
    capture: &ProjectCommitCapture,
) -> Result<mirante4d_project_model::ProjectId, ProjectStoreFault> {
    if capture.expected_parent().is_some() {
        return Err(ProjectStoreFault::StaleParent);
    }
    if capture.autosave_base().is_some() || capture.forked_from().is_some() {
        return Err(ProjectStoreFault::Corruption {
            stage: "initial_provisional_capture",
        });
    }
    Ok(capture.projection().state().project_id())
}

/// Publishes the first manual head into an already prepared, unpublished store
/// root. The package installer above supplies its envelope and final rename;
/// this is still not the public Create command or actor execution path.
pub(crate) fn publish_initial_manual_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    mode: InitialPackageMode,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    root.validate_store_inventory(limits, 0, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    let project_id = capture.projection().state().project_id();
    let envelope = root
        .read_project_envelope(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "project_envelope"))?;
    if envelope.project_id() != project_id {
        return Err(ProjectStoreFault::Corruption {
            stage: "project_envelope_identity",
        });
    }
    validate_initial_capture(mode, &capture)?;
    require_fresh_lane_refs(root, project_id, limits, &mut is_cancelled)?;

    let captured_revision = capture.projection().revision();
    let captured_revision_high_water = capture.projection().revision_high_water().clone();
    let publication = publish_unreferenced_generation(
        root,
        capture,
        GenerationKind::Manual,
        0,
        limits,
        &mut is_cancelled,
    )
    .map_err(map_generation_error)?;

    root.validate_store_inventory(limits, 1, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    require_fresh_lane_refs(root, project_id, limits, &mut is_cancelled)?;
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;
    let head = RefRecord::new(
        RefKind::ManualHead,
        project_id,
        publication.generation_id(),
        None,
        None,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: "initial_manual_head",
    })?;
    if let Err(error) = root.publish_initial_manual_head(head, limits, &mut is_cancelled) {
        if matches!(error, LocalPublicationError::RefCommitIndeterminate) {
            leases.suspend_writes();
        }
        return Err(map_local_error(error, "initial_manual_head"));
    }

    Ok(ProjectStoreReceipt::manual(
        captured_revision,
        captured_revision_high_water,
        publication.generation_id(),
        None,
        publication.created_objects(),
        publication.created_object_bytes(),
    ))
}

/// Publishes the sole base-less autosave head into a prepared unpublished
/// package. The caller owns the sibling-stage install and no-clobber rename.
fn publish_initial_provisional_autosave_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    root.validate_store_inventory(limits, 0, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    let project_id = validate_initial_provisional_capture(&capture)?;
    let envelope = root
        .read_project_envelope(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "project_envelope"))?;
    if envelope.project_id() != project_id {
        return Err(ProjectStoreFault::Corruption {
            stage: "project_envelope_identity",
        });
    }
    require_fresh_lane_refs(root, project_id, limits, &mut is_cancelled)?;

    let captured_revision = capture.projection().revision();
    let captured_revision_high_water = capture.projection().revision_high_water().clone();
    let publication = publish_unreferenced_generation(
        root,
        capture,
        GenerationKind::Autosave,
        0,
        limits,
        &mut is_cancelled,
    )
    .map_err(map_generation_error)?;

    root.validate_store_inventory(limits, 1, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    require_fresh_lane_refs(root, project_id, limits, &mut is_cancelled)?;
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;
    let head = RefRecord::new(
        RefKind::AutosaveHead,
        project_id,
        publication.generation_id(),
        None,
        None,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: "initial_provisional_head",
    })?;
    replace_transaction_ref(
        root,
        leases,
        None,
        head,
        limits,
        &mut is_cancelled,
        "initial_provisional_head",
        TransactionRefPhase::Head,
    )?;

    Ok(ProjectStoreReceipt::autosave(
        captured_revision,
        captured_revision_high_water,
        publication.generation_id(),
        None,
        None,
        publication.created_objects(),
        publication.created_object_bytes(),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitLane {
    Manual,
    Autosave,
}

impl CommitLane {
    const fn generation_kind(self) -> GenerationKind {
        match self {
            Self::Manual => GenerationKind::Manual,
            Self::Autosave => GenerationKind::Autosave,
        }
    }

    const fn head_kind(self) -> RefKind {
        match self {
            Self::Manual => RefKind::ManualHead,
            Self::Autosave => RefKind::AutosaveHead,
        }
    }

    const fn recovery_kind(self) -> RefKind {
        match self {
            Self::Manual => RefKind::ManualRecovery,
            Self::Autosave => RefKind::AutosaveRecovery,
        }
    }

    const fn target(self, inspection: &EstablishedStoreInspection) -> Option<LaneSnapshot> {
        match self {
            Self::Manual => Some(inspection.manual()),
            Self::Autosave => inspection.autosave(),
        }
    }

    const fn head_stage(self) -> &'static str {
        match self {
            Self::Manual => "manual_head",
            Self::Autosave => "autosave_head",
        }
    }

    const fn recovery_stage(self) -> &'static str {
        match self {
            Self::Manual => "manual_recovery",
            Self::Autosave => "autosave_recovery",
        }
    }

    const fn recovery_durability_stage(self) -> &'static str {
        match self {
            Self::Manual => "manual_recovery_durability",
            Self::Autosave => "autosave_recovery_durability",
        }
    }
}

/// Advances one established manual lane through the frozen
/// recovery-before-head protocol. This remains a crate-private transaction
/// primitive; the actor and public ManualSave execution are later B2 work.
pub(crate) fn publish_established_manual_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    publish_established_lane_generation(
        root,
        leases,
        capture,
        CommitLane::Manual,
        limits,
        is_cancelled,
    )
}

/// Publishes a first or advancing autosave for a project with an established
/// manual head. Provisional autosave-only stores and actor coalescing remain
/// later B2 work.
pub(crate) fn publish_established_autosave_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    publish_established_lane_generation(
        root,
        leases,
        capture,
        CommitLane::Autosave,
        limits,
        is_cancelled,
    )
}

/// Advances an autosave-only provisional store without creating a manual
/// authority. It uses the same autosave recovery-before-head protocol as an
/// established project, but every capture, generation, ref, and receipt keeps
/// the manual base absent.
pub(crate) fn publish_provisional_autosave_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    root.validate_store_inventory(limits, 0, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    let project_id = capture.projection().state().project_id();
    let state = inspect_store_state(root, limits, &mut is_cancelled)?;
    if state.project_id() != project_id {
        return Err(ProjectStoreFault::Corruption {
            stage: "project_envelope_identity",
        });
    }
    if is_exact_visible_provisional_commit(&state, &capture) {
        return adopt_visible_provisional_commit(root, leases, capture, limits, &mut is_cancelled);
    }
    let target = validate_provisional_capture(&capture, &state)?;
    let current_generation = state
        .autosave_generation()
        .expect("a validated provisional store has an autosave generation");
    let next_generation_sequence = current_generation
        .generation_sequence()
        .checked_add(1)
        .ok_or(ProjectStoreFault::Capacity {
            stage: "generation_sequence",
        })?;
    let captured_revision = capture.projection().revision();
    let captured_revision_high_water = capture.projection().revision_high_water().clone();
    let expected_autosave = state.autosave();

    let publication = publish_unreferenced_generation(
        root,
        capture,
        GenerationKind::Autosave,
        next_generation_sequence,
        limits,
        &mut is_cancelled,
    )
    .map_err(map_generation_error)?;
    let pending_fixed_refs = usize::from(target.recovery.is_none());
    root.validate_store_inventory(limits, pending_fixed_refs, true, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    require_unchanged_provisional_refs(
        root,
        project_id,
        expected_autosave.expect("a validated provisional store has an autosave lane"),
        limits,
        &mut is_cancelled,
    )?;
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;

    let recovery = RefRecord::new(
        RefKind::AutosaveRecovery,
        project_id,
        target.head.current(),
        None,
        None,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: "autosave_recovery",
    })?;
    replace_transaction_ref(
        root,
        leases,
        target.recovery,
        recovery,
        limits,
        &mut is_cancelled,
        "autosave_recovery",
        TransactionRefPhase::Recovery {
            durability_stage: "autosave_recovery_durability",
        },
    )?;

    // Cancellation here leaves the old autosave head authoritative and the
    // accepted recovery-ahead pair available for an exact fresh retry.
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;
    let head = RefRecord::new(
        RefKind::AutosaveHead,
        project_id,
        publication.generation_id(),
        Some(target.head.current()),
        None,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: "autosave_head",
    })?;
    replace_transaction_ref(
        root,
        leases,
        Some(target.head),
        head,
        limits,
        &mut is_cancelled,
        "autosave_head",
        TransactionRefPhase::Head,
    )?;

    Ok(ProjectStoreReceipt::autosave(
        captured_revision,
        captured_revision_high_water,
        publication.generation_id(),
        Some(target.head.current()),
        None,
        publication.created_objects(),
        publication.created_object_bytes(),
    ))
}

fn is_exact_visible_provisional_commit(
    state: &crate::inspection::StoreStateInspection,
    capture: &ProjectCommitCapture,
) -> bool {
    let Some(expected_parent) = capture.expected_parent() else {
        return false;
    };
    let Some(lane) = state.autosave() else {
        return false;
    };
    let Some(generation) = state.autosave_generation() else {
        return false;
    };
    state.is_provisional()
        && state.manual().is_none()
        && capture.autosave_base().is_none()
        && capture.forked_from().is_none()
        && lane.head.previous() == Some(expected_parent)
        && lane.head.base().is_none()
        && lane.recovery.is_some_and(|recovery| {
            recovery.current() == expected_parent
                && recovery.previous().is_none()
                && recovery.base().is_none()
        })
        && generation.kind() == GenerationKind::Autosave
        && generation.parent_generation_id() == Some(expected_parent)
        && generation.base_manual_generation_id().is_none()
        && generation.forked_from().is_none()
        && generation.projection() == capture.projection()
}

fn adopt_visible_provisional_commit<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let expected_parent = capture
        .expected_parent()
        .expect("visible retry requires its prior autosave head");
    let graph = inspect_store_graph(root, limits, &mut *is_cancelled)?;
    if !is_exact_visible_provisional_commit(graph.state(), &capture) {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let snapshot = graph.snapshot();
    let generation = graph
        .state()
        .autosave_generation()
        .expect("an exact visible retry has one current autosave");
    let parent_bytes = root
        .read_generation_bytes(
            expected_parent,
            limits.generation_bytes_max,
            &mut *is_cancelled,
        )
        .map_err(|error| map_local_error(error, "provisional_retry_parent"))?;
    let parent = GenerationDocument::decode(
        expected_parent,
        graph.state().project_id(),
        &parent_bytes,
        limits,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: "provisional_retry_parent",
    })?;
    if parent.kind() != GenerationKind::Autosave
        || parent.base_manual_generation_id().is_some()
        || parent.forked_from().is_some()
        || generation.generation_sequence()
            != parent
                .generation_sequence()
                .checked_add(1)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "generation_sequence",
                })?
        || generation.projection().revision_high_water().sequence()
            < parent.projection().revision_high_water().sequence()
        || !generation
            .projection()
            .state()
            .dataset()
            .has_same_scientific_content(parent.projection().state().dataset())
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "provisional_retry_continuity",
        });
    }
    crate::full_verify::full_verify(root, limits, &mut *is_cancelled)?;
    let verified = inspect_store_graph(root, limits, &mut *is_cancelled)?;
    if verified.snapshot() != snapshot
        || !is_exact_visible_provisional_commit(verified.state(), &capture)
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    check_cancelled(&mut *is_cancelled).map_err(map_generation_error)?;
    if let Err(error) = root.sync_existing_ref_directory() {
        if matches!(error, LocalPublicationError::RefCommitIndeterminate) {
            leases.suspend_writes();
        }
        return Err(map_local_error(error, "autosave_head"));
    }
    let after_sync = inspect_store_graph(root, limits, || false)?;
    if after_sync.snapshot() != snapshot
        || !is_exact_visible_provisional_commit(after_sync.state(), &capture)
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let generation_id = after_sync
        .state()
        .autosave()
        .expect("an exact visible retry has one autosave lane")
        .head
        .current();
    Ok(ProjectStoreReceipt::autosave(
        capture.projection().revision(),
        capture.projection().revision_high_water().clone(),
        generation_id,
        Some(expected_parent),
        None,
        0,
        0,
    ))
}

fn validate_provisional_capture(
    capture: &ProjectCommitCapture,
    state: &crate::inspection::StoreStateInspection,
) -> Result<LaneSnapshot, ProjectStoreFault> {
    if !state.is_provisional() || state.manual().is_some() {
        return Err(ProjectStoreFault::Corruption {
            stage: "provisional_state",
        });
    }
    let autosave = state.autosave().ok_or(ProjectStoreFault::Corruption {
        stage: "autosave_head",
    })?;
    let generation = state
        .autosave_generation()
        .ok_or(ProjectStoreFault::Corruption {
            stage: "autosave_generation",
        })?;
    if capture.expected_parent() != Some(autosave.head.current()) {
        return Err(ProjectStoreFault::StaleParent);
    }
    if capture.autosave_base().is_some()
        || capture.forked_from().is_some()
        || generation.forked_from().is_some()
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "provisional_capture",
        });
    }
    if !capture
        .projection()
        .state()
        .dataset()
        .has_same_scientific_content(generation.projection().state().dataset())
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "provisional_generation_continuity",
        });
    }
    if capture.projection().revision_high_water().sequence()
        < generation.projection().revision_high_water().sequence()
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "revision_high_water_regression",
        });
    }
    Ok(autosave)
}

fn require_unchanged_provisional_refs<C>(
    root: &LocalStoreRoot,
    project_id: mirante4d_project_model::ProjectId,
    expected_autosave: LaneSnapshot,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let manual = read_lane_snapshot(
        root,
        project_id,
        RefKind::ManualHead,
        RefKind::ManualRecovery,
        limits,
        &mut *is_cancelled,
    )?;
    if manual.is_some() {
        return Err(ProjectStoreFault::Corruption {
            stage: "manual_ref_drift",
        });
    }
    let autosave = read_lane_snapshot(
        root,
        project_id,
        RefKind::AutosaveHead,
        RefKind::AutosaveRecovery,
        limits,
        &mut *is_cancelled,
    )?;
    if autosave.map(|lane| lane.head.current()) != Some(expected_autosave.head.current()) {
        return Err(ProjectStoreFault::StaleParent);
    }
    if autosave != Some(expected_autosave) {
        return Err(ProjectStoreFault::Corruption {
            stage: "lane_ref_drift",
        });
    }
    Ok(())
}

fn publish_established_lane_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    lane: CommitLane,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    root.validate_store_inventory(limits, 0, false, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "store_inventory"))?;
    let project_id = capture.projection().state().project_id();
    let inspection = inspect_established_store(root, limits, &mut is_cancelled)?;
    if inspection.project_id() != project_id {
        return Err(ProjectStoreFault::Corruption {
            stage: "project_envelope_identity",
        });
    }
    validate_established_capture(&capture, lane, &inspection)?;
    let next_generation_sequence = inspection.next_generation_sequence()?;
    let expected_manual = inspection.manual();
    let expected_autosave = inspection.autosave();
    let target = lane.target(&inspection);

    let captured_revision = capture.projection().revision();
    let captured_revision_high_water = capture.projection().revision_high_water().clone();
    let autosave_base = capture.autosave_base();
    let publication = publish_unreferenced_generation(
        root,
        capture,
        lane.generation_kind(),
        next_generation_sequence,
        limits,
        &mut is_cancelled,
    )
    .map_err(map_generation_error)?;

    let (pending_fixed_refs, replaces_fixed_ref) = match target {
        Some(target) => (usize::from(target.recovery.is_none()), true),
        None => (1, false),
    };
    let previous = target.map(|target| target.head.current());
    let receipt = match lane {
        CommitLane::Manual => ProjectStoreReceipt::manual(
            captured_revision,
            captured_revision_high_water,
            publication.generation_id(),
            previous,
            publication.created_objects(),
            publication.created_object_bytes(),
        ),
        CommitLane::Autosave => ProjectStoreReceipt::autosave(
            captured_revision,
            captured_revision_high_water,
            publication.generation_id(),
            previous,
            autosave_base,
            publication.created_objects(),
            publication.created_object_bytes(),
        ),
    };
    root.validate_store_inventory(
        limits,
        pending_fixed_refs,
        replaces_fixed_ref,
        &mut is_cancelled,
    )
    .map_err(|error| map_local_error(error, "store_inventory"))?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    require_unchanged_established_refs(
        root,
        project_id,
        expected_manual,
        expected_autosave,
        lane,
        limits,
        &mut is_cancelled,
    )?;
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;

    if let Some(target) = target {
        let recovery = RefRecord::new(
            lane.recovery_kind(),
            project_id,
            target.head.current(),
            None,
            None,
        )
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: lane.recovery_stage(),
        })?;
        replace_transaction_ref(
            root,
            leases,
            target.recovery,
            recovery,
            limits,
            &mut is_cancelled,
            lane.recovery_stage(),
            TransactionRefPhase::Recovery {
                durability_stage: lane.recovery_durability_stage(),
            },
        )?;

        // Cancellation here deliberately leaves the old lane head
        // authoritative and recovery-ahead available for an exact retry.
        check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;
    }
    let head = RefRecord::new(
        lane.head_kind(),
        project_id,
        publication.generation_id(),
        previous,
        autosave_base,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: lane.head_stage(),
    })?;
    replace_transaction_ref(
        root,
        leases,
        target.map(|target| target.head),
        head,
        limits,
        &mut is_cancelled,
        lane.head_stage(),
        TransactionRefPhase::Head,
    )?;

    Ok(receipt)
}

fn validate_established_capture(
    capture: &ProjectCommitCapture,
    lane: CommitLane,
    inspection: &EstablishedStoreInspection,
) -> Result<(), ProjectStoreFault> {
    let manual = inspection.manual();
    match lane {
        CommitLane::Manual => {
            if capture.autosave_base().is_some() {
                return Err(ProjectStoreFault::Corruption {
                    stage: "established_manual_capture",
                });
            }
            if capture.expected_parent() != Some(manual.head.current()) {
                return Err(ProjectStoreFault::StaleParent);
            }
        }
        CommitLane::Autosave => {
            if capture.autosave_base() != Some(manual.head.current()) {
                return Err(ProjectStoreFault::StaleParent);
            }
        }
    }

    let autosave = inspection.autosave();
    if lane == CommitLane::Autosave
        && capture.expected_parent() != autosave.map(|lane| lane.head.current())
    {
        return Err(ProjectStoreFault::StaleParent);
    }

    let manual_generation = inspection.manual_generation();
    if capture.forked_from() != manual_generation.forked_from()
        || !capture
            .projection()
            .state()
            .dataset()
            .has_same_scientific_content(manual_generation.projection().state().dataset())
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "manual_generation_continuity",
        });
    }
    if capture.projection().revision_high_water().sequence()
        < inspection.maximum_revision_high_water()
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "revision_high_water_regression",
        });
    }
    Ok(())
}

fn require_unchanged_established_refs<C>(
    root: &LocalStoreRoot,
    project_id: mirante4d_project_model::ProjectId,
    expected_manual: LaneSnapshot,
    expected_autosave: Option<LaneSnapshot>,
    lane: CommitLane,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let manual = read_lane_snapshot(
        root,
        project_id,
        RefKind::ManualHead,
        RefKind::ManualRecovery,
        limits,
        &mut *is_cancelled,
    )?
    .ok_or(ProjectStoreFault::StaleParent)?;
    if manual.head.current() != expected_manual.head.current() {
        return Err(ProjectStoreFault::StaleParent);
    }
    let autosave = read_lane_snapshot(
        root,
        project_id,
        RefKind::AutosaveHead,
        RefKind::AutosaveRecovery,
        limits,
        &mut *is_cancelled,
    )?;
    if lane == CommitLane::Autosave
        && autosave.map(|lane| lane.head.current())
            != expected_autosave.map(|lane| lane.head.current())
    {
        return Err(ProjectStoreFault::StaleParent);
    }
    if manual != expected_manual || autosave != expected_autosave {
        return Err(ProjectStoreFault::Corruption {
            stage: "lane_ref_drift",
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransactionRefPhase {
    Recovery { durability_stage: &'static str },
    Head,
}

#[allow(clippy::too_many_arguments)]
fn replace_transaction_ref<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    expected: Option<RefRecord>,
    next: RefRecord,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    stage: &'static str,
    phase: TransactionRefPhase,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    if let Err(error) = root.replace_ref(expected, next, limits, &mut *is_cancelled) {
        return match (error, phase) {
            (LocalPublicationError::RefCommitIndeterminate, TransactionRefPhase::Head) => {
                leases.suspend_writes();
                Err(ProjectStoreFault::CommitIndeterminate)
            }
            (
                LocalPublicationError::RefCommitIndeterminate,
                TransactionRefPhase::Recovery { durability_stage },
            ) => Err(ProjectStoreFault::Corruption {
                stage: durability_stage,
            }),
            (LocalPublicationError::RefChanged, TransactionRefPhase::Head) => {
                Err(ProjectStoreFault::StaleParent)
            }
            (error, _) => Err(map_local_error(error, stage)),
        };
    }
    Ok(())
}

fn require_fresh_lane_refs<C>(
    root: &LocalStoreRoot,
    project_id: mirante4d_project_model::ProjectId,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    if let Some(head) = root
        .read_ref(RefKind::ManualHead, limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "manual_head"))?
    {
        if head.project_id() != project_id {
            return Err(ProjectStoreFault::Corruption {
                stage: "manual_head_identity",
            });
        }
        return Err(ProjectStoreFault::StaleParent);
    }
    for (kind, stage) in [
        (RefKind::ManualRecovery, "manual_recovery"),
        (RefKind::AutosaveHead, "autosave_head"),
        (RefKind::AutosaveRecovery, "autosave_recovery"),
    ] {
        if root
            .read_ref(kind, limits, &mut *is_cancelled)
            .map_err(|error| map_local_error(error, stage))?
            .is_some()
        {
            return Err(ProjectStoreFault::Corruption { stage });
        }
    }
    root.require_empty_ref_namespace(limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "initial_ref_namespace"))?;
    Ok(())
}

fn map_lease_error(error: LeaseError) -> ProjectStoreFault {
    match error {
        LeaseError::Indeterminate => ProjectStoreFault::CommitIndeterminate,
        LeaseError::InvalidAnchor | LeaseError::Io { .. } => ProjectStoreFault::Corruption {
            stage: "writer_lease",
        },
    }
}

fn map_generation_error(error: GenerationTransactionError) -> ProjectStoreFault {
    match error {
        GenerationTransactionError::Cancelled => ProjectStoreFault::Cancelled,
        GenerationTransactionError::SourceChanged | GenerationTransactionError::SourceIo => {
            ProjectStoreFault::SourceChanged
        }
        GenerationTransactionError::Capacity { stage } => ProjectStoreFault::Capacity { stage },
        GenerationTransactionError::Closure | GenerationTransactionError::Codec(_) => {
            ProjectStoreFault::Corruption {
                stage: "generation_closure",
            }
        }
        GenerationTransactionError::Local(error) => map_local_error(error, "generation_io"),
    }
}

fn map_local_error(error: LocalPublicationError, stage: &'static str) -> ProjectStoreFault {
    match error {
        LocalPublicationError::Cancelled => ProjectStoreFault::Cancelled,
        LocalPublicationError::Capacity { .. } => ProjectStoreFault::Capacity { stage },
        LocalPublicationError::SourceLength { .. } => ProjectStoreFault::SourceChanged,
        LocalPublicationError::SourceDigest => ProjectStoreFault::DigestMismatch,
        LocalPublicationError::DestinationExists => ProjectStoreFault::DestinationExists,
        LocalPublicationError::RefAlreadyPresent => ProjectStoreFault::StaleParent,
        LocalPublicationError::RefChanged => ProjectStoreFault::Corruption { stage },
        LocalPublicationError::RefCommitIndeterminate
        | LocalPublicationError::PackageCommitIndeterminate => {
            ProjectStoreFault::CommitIndeterminate
        }
        LocalPublicationError::AtomicPublishUnsupported => ProjectStoreFault::UnsupportedFilesystem,
        LocalPublicationError::InvalidPath
        | LocalPublicationError::ExistingMismatch
        | LocalPublicationError::InvalidGeneration
        | LocalPublicationError::InvalidControl
        | LocalPublicationError::Io { .. } => ProjectStoreFault::Corruption { stage },
    }
}

pub(crate) fn publish_unreferenced_generation<C>(
    root: &LocalStoreRoot,
    capture: ProjectCommitCapture,
    kind: GenerationKind,
    generation_sequence: u64,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<UnreferencedGenerationPublication, GenerationTransactionError>
where
    C: FnMut() -> bool,
{
    let limits = limits
        .validate()
        .map_err(|_| GenerationTransactionError::Capacity {
            stage: "configuration",
        })?;
    check_cancelled(&mut is_cancelled)?;
    let parts = capture.into_parts();

    let logical = unique_logical_descriptors(&parts.projection)?;
    preflight(&logical, limits)?;
    let mut sources = parts
        .object_sources
        .into_iter()
        .map(|source| (source.descriptor().digest(), source))
        .collect::<BTreeMap<_, _>>();
    if sources.len() != logical.len() {
        return Err(GenerationTransactionError::Closure);
    }

    let mut bindings = BTreeMap::new();
    let mut closure = BTreeMap::new();
    let mut created_objects = 0_u64;
    let mut created_object_bytes = 0_u64;

    for (digest, descriptor) in logical {
        check_cancelled(&mut is_cancelled)?;
        let source = sources
            .remove(&digest)
            .ok_or(GenerationTransactionError::Closure)?;
        let storage = if descriptor.byte_length() <= PAGE_BYTES {
            let object = PhysicalObject::new(descriptor.digest(), descriptor.byte_length());
            insert_closure(&mut closure, object, limits)?;
            if !root.durabilize_object_if_present(
                object.digest(),
                object.byte_length(),
                limits.object_or_page_bytes_max,
                &mut is_cancelled,
            )? {
                let mut reader = source
                    .open()
                    .map_err(|_| GenerationTransactionError::SourceIo)?;
                let publication = root.publish_declared_object(
                    &mut reader,
                    object.digest(),
                    object.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut is_cancelled,
                )?;
                record_created(publication, &mut created_objects, &mut created_object_bytes)?;
            }
            ArtifactStorage::direct(&descriptor)?
        } else {
            let pages = derive_page_plan(&source, &descriptor, limits, &mut is_cancelled)?;
            let binding = LogicalObjectBinding::new(descriptor.clone(), pages, limits)?;
            let encoded_binding = binding.encode(limits)?;

            for page in binding.pages() {
                insert_closure(&mut closure, page.object(), limits)?;
            }
            let binding_object = PhysicalObject::new(
                encoded_binding.descriptor().digest(),
                encoded_binding.descriptor().byte_length(),
            );
            insert_closure(&mut closure, binding_object, limits)?;

            let mut reader = source
                .open()
                .map_err(|_| GenerationTransactionError::SourceIo)?;
            for page in binding.pages() {
                let page = page.object();
                let publication = root.publish_consuming_object(
                    &mut reader,
                    page.digest(),
                    page.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut is_cancelled,
                )?;
                record_created(publication, &mut created_objects, &mut created_object_bytes)?;
            }
            require_eof(&mut reader)?;

            let mut binding_reader = Cursor::new(encoded_binding.bytes());
            let publication = root.publish_declared_object(
                &mut binding_reader,
                binding_object.digest(),
                binding_object.byte_length(),
                limits.object_or_page_bytes_max,
                &mut is_cancelled,
            )?;
            record_created(publication, &mut created_objects, &mut created_object_bytes)?;
            ArtifactStorage::paged(&descriptor, encoded_binding.descriptor().clone())?
        };
        bindings.insert(digest, storage);
    }
    if !sources.is_empty() {
        return Err(GenerationTransactionError::Closure);
    }

    let reachable_objects = closure.values().copied().collect::<Vec<_>>();
    let document = GenerationDocument::build_from_projection(
        parts.projection,
        parts.expected_parent,
        parts.autosave_base,
        parts.forked_from,
        kind,
        generation_sequence,
        bindings,
        reachable_objects,
        limits,
    )?;
    validate_physical_closure(root, &document, limits, &mut is_cancelled)?;

    let generation = document.encode(limits)?;
    let checked = GenerationDocument::decode(
        generation.id(),
        document.projection().state().project_id(),
        generation.bytes(),
        limits,
    )?;
    if checked != document {
        return Err(GenerationTransactionError::Closure);
    }
    check_cancelled(&mut is_cancelled)?;
    let generation_publication =
        root.publish_generation(&generation, limits.generation_bytes_max, &mut is_cancelled)?;

    Ok(UnreferencedGenerationPublication {
        document,
        generation,
        generation_disposition: generation_publication.disposition(),
        created_objects,
        created_object_bytes,
    })
}

fn unique_logical_descriptors(
    projection: &mirante4d_project_model::ProjectGenerationProjection,
) -> Result<BTreeMap<ExactBytesDigest, RawObjectDescriptor>, GenerationTransactionError> {
    let mut descriptors = BTreeMap::new();
    for artifact in projection.state().artifacts() {
        match descriptors.entry(artifact.object().digest()) {
            Entry::Vacant(entry) => {
                entry.insert(artifact.object().clone());
            }
            Entry::Occupied(entry) if entry.get() == artifact.object() => {}
            Entry::Occupied(_) => return Err(GenerationTransactionError::Closure),
        }
    }
    Ok(descriptors)
}

fn preflight(
    logical: &BTreeMap<ExactBytesDigest, RawObjectDescriptor>,
    limits: ProjectStoreLimits,
) -> Result<(), GenerationTransactionError> {
    if logical.len() > limits.artifact_records_per_generation_max {
        return Err(GenerationTransactionError::Capacity {
            stage: "logical objects",
        });
    }
    let mut minimum_closure = 0_usize;
    for descriptor in logical.values() {
        if descriptor.byte_length() <= PAGE_BYTES {
            minimum_closure =
                minimum_closure
                    .checked_add(1)
                    .ok_or(GenerationTransactionError::Capacity {
                        stage: "physical closure",
                    })?;
        } else {
            let page_count = descriptor.byte_length().div_ceil(PAGE_BYTES);
            if page_count > PAGE_RECORDS_MAX {
                return Err(GenerationTransactionError::Capacity {
                    stage: "logical object pages",
                });
            }
            minimum_closure =
                minimum_closure
                    .checked_add(2)
                    .ok_or(GenerationTransactionError::Capacity {
                        stage: "physical closure",
                    })?;
            if limits.object_or_page_bytes_max < PAGE_BYTES {
                return Err(GenerationTransactionError::Capacity {
                    stage: "page geometry",
                });
            }
        }
    }
    if minimum_closure > limits.reachable_objects_per_generation_max {
        return Err(GenerationTransactionError::Capacity {
            stage: "physical closure",
        });
    }
    Ok(())
}

fn derive_page_plan<C>(
    source: &CapturedObjectSource,
    logical: &RawObjectDescriptor,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<Vec<LogicalObjectPage>, GenerationTransactionError>
where
    C: FnMut() -> bool,
{
    let page_count = logical.byte_length().div_ceil(PAGE_BYTES);
    if page_count == 0 || page_count > PAGE_RECORDS_MAX {
        return Err(GenerationTransactionError::Capacity {
            stage: "logical object pages",
        });
    }
    let page_capacity =
        usize::try_from(page_count).map_err(|_| GenerationTransactionError::Capacity {
            stage: "logical object pages",
        })?;
    let buffer_bytes = limits.stream_buffer_bytes_max.min(STREAM_BUFFER_BYTES_MAX);
    let mut buffer = vec![0_u8; buffer_bytes];
    let mut reader = source
        .open()
        .map_err(|_| GenerationTransactionError::SourceIo)?;
    let mut logical_hasher = ExactBytesHasher::new();
    let mut pages = Vec::with_capacity(page_capacity);
    let mut remaining = logical.byte_length();
    let mut offset = 0_u64;
    for ordinal in 0..page_count {
        let page_bytes = remaining.min(PAGE_BYTES);
        let mut page_hasher = ExactBytesHasher::new();
        let mut page_remaining = page_bytes;
        while page_remaining != 0 {
            check_cancelled(is_cancelled)?;
            let request = usize::try_from(page_remaining.min(buffer.len() as u64))
                .expect("the request is bounded by the allocated buffer");
            let read = read_some(&mut reader, &mut buffer[..request])?;
            if read == 0 {
                return Err(GenerationTransactionError::SourceChanged);
            }
            page_hasher
                .update(&buffer[..read])
                .map_err(|_| GenerationTransactionError::SourceChanged)?;
            logical_hasher
                .update(&buffer[..read])
                .map_err(|_| GenerationTransactionError::SourceChanged)?;
            page_remaining -= u64::try_from(read).expect("a read byte count fits u64");
        }
        let facts = page_hasher
            .finalize()
            .map_err(|_| GenerationTransactionError::SourceChanged)?;
        pages.push(LogicalObjectPage::new(
            u32::try_from(ordinal).map_err(|_| GenerationTransactionError::Capacity {
                stage: "page ordinal",
            })?,
            offset,
            PhysicalObject::new(facts.digest(), facts.byte_length()),
        ));
        offset = offset
            .checked_add(page_bytes)
            .ok_or(GenerationTransactionError::Capacity {
                stage: "page offsets",
            })?;
        remaining -= page_bytes;
    }
    require_eof(&mut reader)?;
    let facts = logical_hasher
        .finalize()
        .map_err(|_| GenerationTransactionError::SourceChanged)?;
    if facts.digest() != logical.digest() || facts.byte_length() != logical.byte_length() {
        return Err(GenerationTransactionError::SourceChanged);
    }
    Ok(pages)
}

fn validate_physical_closure<C>(
    root: &LocalStoreRoot,
    document: &GenerationDocument,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), GenerationTransactionError>
where
    C: FnMut() -> bool,
{
    let logical = unique_logical_descriptors(document.projection())?;
    let mut observed = BTreeMap::new();
    for (digest, storage) in document.bindings() {
        let descriptor = logical
            .get(digest)
            .ok_or(GenerationTransactionError::Closure)?;
        match storage {
            ArtifactStorage::Direct { object } => {
                root.read_exact_object(
                    object.digest(),
                    object.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut *is_cancelled,
                    |_| {},
                )?;
                insert_closure(&mut observed, *object, limits)?;
            }
            ArtifactStorage::Paged { binding_manifest } => {
                let capacity = usize::try_from(binding_manifest.byte_length()).map_err(|_| {
                    GenerationTransactionError::Capacity {
                        stage: "binding bytes",
                    }
                })?;
                let mut binding_bytes = Vec::with_capacity(capacity);
                root.read_exact_object(
                    binding_manifest.digest(),
                    binding_manifest.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut *is_cancelled,
                    |bytes| binding_bytes.extend_from_slice(bytes),
                )?;
                let binding = LogicalObjectBinding::decode(
                    &binding_bytes,
                    descriptor,
                    binding_manifest,
                    limits,
                )?;
                let mut logical_hasher = ExactBytesHasher::new();
                for page in binding.pages() {
                    let page = page.object();
                    root.read_exact_object(
                        page.digest(),
                        page.byte_length(),
                        limits.object_or_page_bytes_max,
                        &mut *is_cancelled,
                        |bytes| {
                            logical_hasher
                                .update(bytes)
                                .expect("validated page closure cannot overflow u64")
                        },
                    )?;
                    insert_closure(&mut observed, page, limits)?;
                }
                let facts = logical_hasher
                    .finalize()
                    .map_err(|_| GenerationTransactionError::Closure)?;
                if facts.digest() != descriptor.digest()
                    || facts.byte_length() != descriptor.byte_length()
                {
                    return Err(GenerationTransactionError::Closure);
                }
                insert_closure(
                    &mut observed,
                    PhysicalObject::new(binding_manifest.digest(), binding_manifest.byte_length()),
                    limits,
                )?;
            }
        }
    }
    let observed = observed.values().copied().collect::<Vec<_>>();
    if observed != document.reachable_objects() {
        return Err(GenerationTransactionError::Closure);
    }
    Ok(())
}

fn insert_closure(
    closure: &mut BTreeMap<ExactBytesDigest, PhysicalObject>,
    object: PhysicalObject,
    limits: ProjectStoreLimits,
) -> Result<(), GenerationTransactionError> {
    if let Some(existing) = closure.get(&object.digest()) {
        return if *existing == object {
            Ok(())
        } else {
            Err(GenerationTransactionError::Closure)
        };
    }
    if closure.len() >= limits.reachable_objects_per_generation_max {
        return Err(GenerationTransactionError::Capacity {
            stage: "physical closure",
        });
    }
    closure.insert(object.digest(), object);
    Ok(())
}

fn record_created(
    publication: ImmutablePublication,
    created_objects: &mut u64,
    created_bytes: &mut u64,
) -> Result<(), GenerationTransactionError> {
    if publication.disposition() == PublicationDisposition::Created {
        *created_objects =
            created_objects
                .checked_add(1)
                .ok_or(GenerationTransactionError::Capacity {
                    stage: "publication counters",
                })?;
        *created_bytes = created_bytes
            .checked_add(publication.facts().byte_length())
            .ok_or(GenerationTransactionError::Capacity {
                stage: "publication counters",
            })?;
    }
    Ok(())
}

fn check_cancelled(
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), GenerationTransactionError> {
    if is_cancelled() {
        Err(GenerationTransactionError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_some(
    reader: &mut dyn Read,
    buffer: &mut [u8],
) -> Result<usize, GenerationTransactionError> {
    loop {
        match reader.read(buffer) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(GenerationTransactionError::SourceIo),
        }
    }
}

fn require_eof(reader: &mut dyn Read) -> Result<(), GenerationTransactionError> {
    let mut byte = [0_u8; 1];
    if read_some(reader, &mut byte)? == 0 {
        Ok(())
    } else {
        Err(GenerationTransactionError::SourceChanged)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Cursor,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, IsoLightState, LayerTransfer, LogicalLayerKey,
        Opacity, Projection, RenderState, RgbColor, SamplingPolicy, TimeIndex, TransferCurve,
        UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_identity::{
        ArtifactContentId, ExactBytesHasher, MediaType, ObjectRole, ScientificContentId,
    };
    use mirante4d_project_model::{
        ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
        ArtifactSchema, DatasetReference, LayerViewState, ProjectGenerationProjection, ProjectId,
        ProjectRevisionHighWater, ProjectRevisionId, ProjectState, ViewState,
    };

    use super::*;
    use crate::{ProjectObjectSource, ProjectOpenMode, wire::ProjectEnvelope};

    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-project-transaction-{label}-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct MemorySource {
        descriptor: RawObjectDescriptor,
        bytes: Arc<[u8]>,
        opens: Arc<AtomicUsize>,
        flip_second: bool,
    }

    impl ProjectObjectSource for MemorySource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            let open = self.opens.fetch_add(1, Ordering::SeqCst);
            let mut bytes = self.bytes.to_vec();
            if self.flip_second && open == 1 && !bytes.is_empty() {
                bytes[0] ^= 1;
            }
            Ok(Box::new(Cursor::new(bytes)))
        }
    }

    struct MustNotOpen {
        descriptor: RawObjectDescriptor,
    }

    impl ProjectObjectSource for MustNotOpen {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            panic!("preflight opened a rejected source")
        }
    }

    struct RepeatedPageSource {
        descriptor: RawObjectDescriptor,
        page: Arc<[u8]>,
        opens: Arc<AtomicUsize>,
    }

    const STALE_PROJECT: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    const STALE_GENERATION: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d5020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec"
    );
    const STALE_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "c357ffd5f7c051bf22877ffcd6680bdcd0f7db4068af93587e4a1f5bed0542a0"
    );
    const RECOVERABLE_G1: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "9cf3985edc9a7de3702029a4b32fd3e4188796ee8459deddd0c6cd7babf57d81"
    );
    const RECOVERABLE_G2: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
    );
    const RECOVERABLE_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "cfd67414728bb345edb7d5eabffac2530f04ed3b768d720782efe88e2d7ca370"
    );
    const RECOVERABLE_A1: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "dc1669b5773f1708b72114fb171e69c92d551e946de567ddd30d0a7c9a19d63c"
    );
    const RECOVERABLE_A2: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d9504b896fd6a3fb21e52d227fcd284df654d4f063ea8ee0ca49fce0155e9b73"
    );
    const DIVERGENT_MANUAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "6b91b33dbaa378598269005b027db7a0643e14babe4b7522a5a415a461f6a497"
    );
    const DIVERGENT_INITIAL: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "10011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10"
    );
    const DIVERGENT_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "b9af2901b12b248533e53d2683fcf4db7d4b2eb33ef292413b8b5dc2cb8b951e"
    );
    const PROVISIONAL_AUTOSAVE: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "a1a84e1b98686c1d9eda416177988e691695baed74244ff5b99136e839ab0cea"
    );

    impl ProjectObjectSource for RepeatedPageSource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(
                Cursor::new(self.page.clone()).chain(Cursor::new(self.page.clone())),
            ))
        }
    }

    #[test]
    fn two_pass_paging_publishes_the_exact_closure_before_the_generation() {
        let directory = TestDirectory::new("success");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let generation_member = concat!(
            "recoverable.m4dproj/generations/sha256/50/",
            "fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854.json"
        );
        let generation_bytes = fixture_extract(generation_member);
        let expected_id = ProjectGenerationId::parse(concat!(
            "m4d-project-generation-v1-sha256:",
            "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
        ))
        .unwrap();
        let project_id = ProjectId::parse("11111111-2222-4333-8444-555555555555").unwrap();
        let frozen = GenerationDocument::decode(
            expected_id,
            project_id,
            &generation_bytes,
            ProjectStoreLimits::default(),
        )
        .unwrap();

        let direct_opens = Arc::new(AtomicUsize::new(0));
        let paged_opens = Arc::new(AtomicUsize::new(0));
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        for artifact in frozen.projection().state().artifacts() {
            let storage = frozen.bindings().get(&artifact.object().digest()).unwrap();
            let (bytes, opens) = match storage {
                ArtifactStorage::Direct { object } => (
                    Arc::<[u8]>::from(fixture_extract(&fixture_object_member(
                        "recoverable.m4dproj",
                        object.digest(),
                    ))),
                    Arc::clone(&direct_opens),
                ),
                ArtifactStorage::Paged { binding_manifest } => {
                    let binding_bytes = fixture_extract(&fixture_object_member(
                        "recoverable.m4dproj",
                        binding_manifest.digest(),
                    ));
                    let binding = LogicalObjectBinding::decode(
                        &binding_bytes,
                        artifact.object(),
                        binding_manifest,
                        ProjectStoreLimits::default(),
                    )
                    .unwrap();
                    let mut logical = Vec::with_capacity(
                        usize::try_from(artifact.object().byte_length()).unwrap(),
                    );
                    for page in binding.pages() {
                        logical.extend_from_slice(&fixture_extract(&fixture_object_member(
                            "recoverable.m4dproj",
                            page.object().digest(),
                        )));
                    }
                    (Arc::<[u8]>::from(logical), Arc::clone(&paged_opens))
                }
            };
            sources.push(Box::new(MemorySource {
                descriptor: artifact.object().clone(),
                bytes,
                opens,
                flip_second: false,
            }));
        }
        let capture = ProjectCommitCapture::new(
            frozen.projection().clone(),
            frozen.parent_generation_id(),
            frozen.base_manual_generation_id(),
            frozen.forked_from(),
            sources,
        )
        .unwrap();

        let published = publish_unreferenced_generation(
            &root,
            capture,
            frozen.kind(),
            frozen.generation_sequence(),
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(published.generation_id(), expected_id);
        assert_eq!(published.generation_bytes(), generation_bytes);
        assert_eq!(published.document(), &frozen);
        assert_eq!(direct_opens.load(Ordering::SeqCst), 1);
        assert_eq!(paged_opens.load(Ordering::SeqCst), 2);
        assert_eq!(published.document().reachable_objects().len(), 4);
        assert_eq!(published.created_objects(), 4);
        assert!(published.created_object_bytes() > PAGE_BYTES);
        assert_eq!(
            published.generation_disposition(),
            PublicationDisposition::Created
        );
        assert_eq!(
            fs::read(generation_path(directory.path(), published.generation_id())).unwrap(),
            published.generation_bytes()
        );
    }

    #[test]
    fn preflight_and_second_pass_drift_publish_no_generation() {
        let huge_directory = TestDirectory::new("preflight");
        let huge_root = LocalStoreRoot::open(huge_directory.path()).unwrap();
        let huge_length = PAGE_BYTES * (PAGE_RECORDS_MAX + 1);
        let huge_descriptor = RawObjectDescriptor::new(
            mirante4d_identity::ExactBytesDigest::from_digest(
                mirante4d_identity::Sha256Digest::from_bytes([7; 32]),
            ),
            huge_length,
            MediaType::parse(ArtifactSchema::AnalysisTableV1.media_type()).unwrap(),
            ObjectRole::parse(ArtifactSchema::AnalysisTableV1.object_role()).unwrap(),
        );
        let huge_capture = capture(
            vec![artifact(
                3,
                ArtifactSchema::AnalysisTableV1,
                huge_descriptor.clone(),
            )],
            vec![Box::new(MustNotOpen {
                descriptor: huge_descriptor,
            })],
        );
        assert!(matches!(
            publish_unreferenced_generation(
                &huge_root,
                huge_capture,
                GenerationKind::Manual,
                0,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(GenerationTransactionError::Capacity { .. })
        ));
        assert!(!huge_directory.path().join("generations").exists());

        let drift_directory = TestDirectory::new("drift");
        let drift_root = LocalStoreRoot::open(drift_directory.path()).unwrap();
        let bytes = Arc::<[u8]>::from(vec![19_u8; usize::try_from(PAGE_BYTES).unwrap() + 1]);
        let drift_descriptor = descriptor(ArtifactSchema::AnalysisTableV1, &bytes);
        let drift_capture = capture(
            vec![artifact(
                4,
                ArtifactSchema::AnalysisTableV1,
                drift_descriptor.clone(),
            )],
            vec![Box::new(MemorySource {
                descriptor: drift_descriptor,
                bytes,
                opens: Arc::new(AtomicUsize::new(0)),
                flip_second: true,
            })],
        );
        assert!(matches!(
            publish_unreferenced_generation(
                &drift_root,
                drift_capture,
                GenerationKind::Manual,
                0,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(GenerationTransactionError::SourceChanged)
        ));
        assert!(!drift_directory.path().join("generations").exists());

        let cancelled_directory = TestDirectory::new("cancelled");
        let cancelled_root = LocalStoreRoot::open(cancelled_directory.path()).unwrap();
        let cancelled_bytes =
            Arc::<[u8]>::from(vec![29_u8; usize::try_from(PAGE_BYTES).unwrap() + 1]);
        let cancelled_descriptor = descriptor(ArtifactSchema::AnalysisTableV1, &cancelled_bytes);
        let cancelled_opens = Arc::new(AtomicUsize::new(0));
        let cancelled_capture = capture(
            vec![artifact(
                6,
                ArtifactSchema::AnalysisTableV1,
                cancelled_descriptor.clone(),
            )],
            vec![Box::new(MemorySource {
                descriptor: cancelled_descriptor,
                bytes: cancelled_bytes,
                opens: Arc::clone(&cancelled_opens),
                flip_second: false,
            })],
        );
        let mut polls = 0_u8;
        assert!(matches!(
            publish_unreferenced_generation(
                &cancelled_root,
                cancelled_capture,
                GenerationKind::Manual,
                0,
                ProjectStoreLimits::default(),
                || {
                    polls += 1;
                    polls == 5
                },
            ),
            Err(GenerationTransactionError::Cancelled)
        ));
        assert_eq!(polls, 5);
        assert_eq!(cancelled_opens.load(Ordering::SeqCst), 1);
        assert!(!cancelled_directory.path().join("objects").exists());
        assert!(!cancelled_directory.path().join("generations").exists());
    }

    #[test]
    fn closure_union_reuses_identical_physical_objects() {
        let directory = TestDirectory::new("repeated-page");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let page = Arc::<[u8]>::from(vec![23_u8; usize::try_from(PAGE_BYTES).unwrap()]);
        let mut logical_hasher = ExactBytesHasher::new();
        logical_hasher.update(&page).unwrap();
        logical_hasher.update(&page).unwrap();
        let logical = logical_hasher.finalize().unwrap();
        let descriptor = RawObjectDescriptor::new(
            logical.digest(),
            logical.byte_length(),
            MediaType::parse(ArtifactSchema::AnalysisTableV1.media_type()).unwrap(),
            ObjectRole::parse(ArtifactSchema::AnalysisTableV1.object_role()).unwrap(),
        );
        let opens = Arc::new(AtomicUsize::new(0));
        let capture = capture(
            vec![artifact(
                5,
                ArtifactSchema::AnalysisTableV1,
                descriptor.clone(),
            )],
            vec![Box::new(RepeatedPageSource {
                descriptor,
                page,
                opens: Arc::clone(&opens),
            })],
        );
        let published = publish_unreferenced_generation(
            &root,
            capture,
            GenerationKind::Manual,
            0,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(published.document().reachable_objects().len(), 2);
        assert_eq!(published.created_objects(), 2);
    }

    #[test]
    fn first_manual_commit_matches_the_frozen_initial_head() {
        let directory = prepared_frozen_root("initial-success");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let (frozen_id, frozen, generation_bytes) = frozen_stale_manual();
        let (capture, opens) = frozen_capture("stale.m4dproj", &frozen);

        let receipt = publish_initial_manual_generation(
            &root,
            &leases,
            capture,
            InitialPackageMode::Create,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.new_generation_id(), frozen_id);
        assert_eq!(receipt.current_generation_id(), frozen_id);
        assert_eq!(receipt.previous_generation_id(), None);
        assert_eq!(receipt.autosave_base_generation_id(), None);
        assert_eq!(receipt.captured_revision(), frozen.projection().revision());
        assert_eq!(
            receipt.captured_revision_high_water(),
            frozen.projection().revision_high_water()
        );
        assert_eq!(
            receipt.published_objects(),
            u64::try_from(frozen.reachable_objects().len()).unwrap()
        );
        assert_eq!(
            fs::read(generation_path(directory.path(), frozen_id)).unwrap(),
            generation_bytes
        );
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            fixture_extract("stale.m4dproj/refs/head")
        );
        assert!(!directory.path().join("refs/recovery").exists());
        assert!(!directory.path().join("refs/autosave-head").exists());
        assert!(!directory.path().join("refs/autosave-recovery").exists());
        assert!(opens.load(Ordering::SeqCst) >= frozen.projection().state().artifacts().len());
    }

    #[test]
    fn initial_package_install_supports_create_and_explicit_save_as_forks() {
        let create_parent = TestDirectory::new("package-create");
        let create_destination =
            ProjectStorePath::new(create_parent.path().join("created.m4dproj")).unwrap();
        let (create_id, create_generation, _) = frozen_stale_manual();
        let (create_capture, _) = frozen_capture("stale.m4dproj", &create_generation);
        let create_limits = ProjectStoreLimits {
            open_file_descriptors_max: 9,
            ..ProjectStoreLimits::default()
        };
        let created = install_initial_manual_package(
            &create_destination,
            InitialPackageMode::Create,
            create_capture,
            create_limits,
            || false,
        )
        .unwrap();
        assert_eq!(created.receipt().new_generation_id(), create_id);
        let (created_root, created_leases, created_receipt) = created.into_parts();
        assert!(created_leases.confirm_writer(&created_root).unwrap());
        let created_inspection =
            inspect_established_store(&created_root, ProjectStoreLimits::default(), || false)
                .unwrap();
        assert_eq!(
            created_inspection.manual().head.current(),
            created_receipt.current_generation_id()
        );

        let save_as_parent = TestDirectory::new("package-save-as");
        let save_as_destination =
            ProjectStorePath::new(save_as_parent.path().join("fork.m4dproj")).unwrap();
        let fork_generation = frozen_generation("divergent.m4dproj", DIVERGENT_INITIAL);
        let fork = fork_generation.forked_from().unwrap();
        let (save_as_capture, _) = frozen_capture("divergent.m4dproj", &fork_generation);
        let saved_as = install_initial_manual_package(
            &save_as_destination,
            InitialPackageMode::SaveAs {
                source_project_id: fork.0,
                source_generation_id: fork.1,
            },
            save_as_capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(
            saved_as.receipt().new_generation_id(),
            ProjectGenerationId::parse(DIVERGENT_INITIAL).unwrap()
        );
        let installed_generation = GenerationDocument::decode(
            saved_as.receipt().new_generation_id(),
            fork_generation.projection().state().project_id(),
            &fs::read(generation_path(
                save_as_destination.as_path(),
                saved_as.receipt().new_generation_id(),
            ))
            .unwrap(),
            ProjectStoreLimits::default(),
        )
        .unwrap();
        assert_eq!(installed_generation.forked_from(), Some(fork));
    }

    #[test]
    fn package_collision_and_cancellation_leave_no_new_authority() {
        let collision_parent = TestDirectory::new("package-collision");
        let collision_destination =
            ProjectStorePath::new(collision_parent.path().join("exists.m4dproj")).unwrap();
        fs::create_dir(collision_destination.as_path()).unwrap();
        fs::write(collision_destination.as_path().join("marker"), b"owned").unwrap();
        let (_, frozen, _) = frozen_stale_manual();
        let (collision_capture, opens) = frozen_capture("stale.m4dproj", &frozen);
        assert!(matches!(
            install_initial_manual_package(
                &collision_destination,
                InitialPackageMode::Create,
                collision_capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::DestinationExists)
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(collision_destination.as_path().join("marker")).unwrap(),
            b"owned"
        );

        let file_destination =
            ProjectStorePath::new(collision_parent.path().join("file.m4dproj")).unwrap();
        fs::write(file_destination.as_path(), b"unrelated").unwrap();
        let (file_capture, file_opens) = frozen_capture("stale.m4dproj", &frozen);
        assert!(matches!(
            install_initial_manual_package(
                &file_destination,
                InitialPackageMode::Create,
                file_capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::DestinationExists)
        ));
        assert_eq!(file_opens.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(file_destination.as_path()).unwrap(), b"unrelated");

        let symlink_destination =
            ProjectStorePath::new(collision_parent.path().join("link.m4dproj")).unwrap();
        symlink(
            collision_destination.as_path(),
            symlink_destination.as_path(),
        )
        .unwrap();
        let (symlink_capture, symlink_opens) = frozen_capture("stale.m4dproj", &frozen);
        assert!(matches!(
            install_initial_manual_package(
                &symlink_destination,
                InitialPackageMode::Create,
                symlink_capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::DestinationExists)
        ));
        assert_eq!(symlink_opens.load(Ordering::SeqCst), 0);
        assert!(
            fs::symlink_metadata(symlink_destination.as_path())
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let bounded_parent = TestDirectory::new("package-descriptor-bound");
        let bounded_destination =
            ProjectStorePath::new(bounded_parent.path().join("bounded.m4dproj")).unwrap();
        let (bounded_capture, bounded_opens) = frozen_capture("stale.m4dproj", &frozen);
        let bounded_limits = ProjectStoreLimits {
            open_file_descriptors_max: 8,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            install_initial_manual_package(
                &bounded_destination,
                InitialPackageMode::Create,
                bounded_capture,
                bounded_limits,
                || false,
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "store_inventory"
            })
        ));
        assert!(bounded_opens.load(Ordering::SeqCst) > 0);
        assert!(!bounded_destination.as_path().exists());
        assert_eq!(fs::read_dir(bounded_parent.path()).unwrap().count(), 0);

        let tiny_parent = TestDirectory::new("package-tiny-inventory");
        let tiny_destination =
            ProjectStorePath::new(tiny_parent.path().join("tiny.m4dproj")).unwrap();
        let (tiny_capture, tiny_opens) = frozen_capture("stale.m4dproj", &frozen);
        let tiny_limits = ProjectStoreLimits {
            physical_store_entries_max: 1,
            directory_fanout_entries_max: 1,
            ..ProjectStoreLimits::default()
        };
        assert!(matches!(
            install_initial_manual_package(
                &tiny_destination,
                InitialPackageMode::Create,
                tiny_capture,
                tiny_limits,
                || false,
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "store_inventory"
            })
        ));
        assert!(tiny_opens.load(Ordering::SeqCst) > 0);
        assert!(!tiny_destination.as_path().exists());
        assert_eq!(fs::read_dir(tiny_parent.path()).unwrap().count(), 0);

        let cancel_parent = TestDirectory::new("package-cancel");
        let cancel_destination =
            ProjectStorePath::new(cancel_parent.path().join("cancelled.m4dproj")).unwrap();
        let (cancel_capture, _) = frozen_capture("stale.m4dproj", &frozen);
        let mut saw_populated_stage = false;
        assert!(matches!(
            install_initial_manual_package(
                &cancel_destination,
                InitialPackageMode::Create,
                cancel_capture,
                ProjectStoreLimits::default(),
                || {
                    let populated = fs::read_dir(cancel_parent.path()).unwrap().any(|entry| {
                        let stage = entry.unwrap().path();
                        stage
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(".mirante4d-project-stage-"))
                            && stage.join("refs/head").is_file()
                    });
                    saw_populated_stage |= populated;
                    populated
                },
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert!(saw_populated_stage);
        assert!(!cancel_destination.as_path().exists());
        assert_eq!(fs::read_dir(cancel_parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn post_install_parent_sync_failure_is_indeterminate_and_never_deleted() {
        let parent = TestDirectory::new("package-indeterminate");
        let destination = ProjectStorePath::new(parent.path().join("visible.m4dproj")).unwrap();
        let (generation_id, frozen, _) = frozen_stale_manual();
        let (capture, _) = frozen_capture("stale.m4dproj", &frozen);
        assert!(matches!(
            install_initial_manual_package_with_parent_sync_failure(
                &destination,
                InitialPackageMode::Create,
                capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));
        assert!(destination.as_path().is_dir());
        let root = LocalStoreRoot::open(destination.as_path()).unwrap();
        let inspection =
            inspect_established_store(&root, ProjectStoreLimits::default(), || false).unwrap();
        assert_eq!(inspection.manual().head.current(), generation_id);
    }

    #[test]
    fn first_provisional_autosave_installs_the_exact_base_less_fixture() {
        let parent = TestDirectory::new("provisional-first");
        let destination = ProjectStorePath::new(parent.path().join("unsaved.m4dproj")).unwrap();
        let frozen = frozen_generation("provisional.m4dproj", PROVISIONAL_AUTOSAVE);
        let generation_id = ProjectGenerationId::parse(PROVISIONAL_AUTOSAVE).unwrap();
        let (capture, opens) = frozen_capture("provisional.m4dproj", &frozen);

        let installed = install_initial_provisional_autosave_package(
            &destination,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(installed.receipt().new_generation_id(), generation_id);
        assert_eq!(installed.receipt().current_generation_id(), generation_id);
        assert_eq!(installed.receipt().previous_generation_id(), None);
        assert_eq!(installed.receipt().autosave_base_generation_id(), None);
        assert!(opens.load(Ordering::SeqCst) > 0);
        assert_eq!(
            fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
            fixture_extract("provisional.m4dproj/refs/autosave-head")
        );
        assert_eq!(
            fs::read(generation_path(destination.as_path(), generation_id)).unwrap(),
            fixture_extract(&fixture_generation_member(
                "provisional.m4dproj",
                generation_id,
            ))
        );
        assert!(!destination.as_path().join("refs/head").exists());
        assert!(!destination.as_path().join("refs/recovery").exists());
        assert!(
            !destination
                .as_path()
                .join("refs/autosave-recovery")
                .exists()
        );

        let (root, leases, _) = installed.into_parts();
        assert!(leases.confirm_writer(&root).unwrap());
        let graph = inspect_store_graph(&root, ProjectStoreLimits::default(), || false).unwrap();
        assert!(graph.state().is_provisional());
        assert_eq!(graph.generation_ids(), [generation_id]);
        assert_eq!(graph.root_generation_ids(), [generation_id]);
        assert!(graph.orphan_generation_ids().is_empty());
    }

    #[test]
    fn provisional_first_publication_cancels_cleans_and_exactly_adopts_uncertain_install() {
        let frozen = frozen_generation("provisional.m4dproj", PROVISIONAL_AUTOSAVE);

        let cancel_parent = TestDirectory::new("provisional-first-cancel");
        let cancel_destination =
            ProjectStorePath::new(cancel_parent.path().join("cancelled.m4dproj")).unwrap();
        let (cancel_capture, _) = frozen_capture("provisional.m4dproj", &frozen);
        let mut saw_complete_stage = false;
        assert!(matches!(
            install_initial_provisional_autosave_package(
                &cancel_destination,
                cancel_capture,
                ProjectStoreLimits::default(),
                || {
                    let complete = fs::read_dir(cancel_parent.path()).unwrap().any(|entry| {
                        let stage = entry.unwrap().path();
                        stage
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(".mirante4d-project-stage-"))
                            && stage.join("refs/autosave-head").is_file()
                    });
                    saw_complete_stage |= complete;
                    complete
                },
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert!(saw_complete_stage);
        assert!(!cancel_destination.as_path().exists());
        assert_eq!(fs::read_dir(cancel_parent.path()).unwrap().count(), 0);

        let retry_parent = TestDirectory::new("provisional-first-retry");
        let retry_destination =
            ProjectStorePath::new(retry_parent.path().join("visible.m4dproj")).unwrap();
        let (uncertain_capture, _) = frozen_capture("provisional.m4dproj", &frozen);
        assert!(matches!(
            install_initial_provisional_autosave_package_with_parent_sync_failure(
                &retry_destination,
                uncertain_capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));
        assert!(retry_destination.as_path().is_dir());

        let (retry_capture, retry_opens) = frozen_capture("provisional.m4dproj", &frozen);
        let adopted = install_initial_provisional_autosave_package(
            &retry_destination,
            retry_capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(retry_opens.load(Ordering::SeqCst), 0);
        assert_eq!(adopted.receipt().published_objects(), 0);
        assert_eq!(adopted.receipt().published_bytes(), 0);
        assert_eq!(adopted.receipt().autosave_base_generation_id(), None);
        let (_, adopted_leases, _) = adopted.into_parts();
        drop(adopted_leases);

        let head_before_foreign =
            fs::read(retry_destination.as_path().join("refs/autosave-head")).unwrap();
        fs::write(
            retry_destination.as_path().join("foreign-entry"),
            b"foreign",
        )
        .unwrap();
        let (foreign_retry, foreign_opens) = frozen_capture("provisional.m4dproj", &frozen);
        assert!(matches!(
            install_initial_provisional_autosave_package(
                &retry_destination,
                foreign_retry,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "initial_package_namespace"
            })
        ));
        assert_eq!(foreign_opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(retry_destination.as_path().join("foreign-entry")).unwrap(),
            b"foreign"
        );
        assert_eq!(
            fs::read(retry_destination.as_path().join("refs/autosave-head")).unwrap(),
            head_before_foreign
        );
        fs::remove_file(retry_destination.as_path().join("foreign-entry")).unwrap();

        let next_revision = frozen.projection().revision().sequence() + 1;
        let next_high_water = frozen.projection().revision_high_water().sequence() + 1;
        let mismatched_projection =
            projection_with_revision(&frozen, next_revision, next_high_water);
        let (mismatched, mismatched_opens) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            mismatched_projection,
            None,
            None,
        );
        assert!(matches!(
            install_initial_provisional_autosave_package(
                &retry_destination,
                mismatched,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::DestinationExists)
        ));
        assert_eq!(mismatched_opens.load(Ordering::SeqCst), 0);

        let race_parent = TestDirectory::new("provisional-first-race");
        let race_destination =
            ProjectStorePath::new(race_parent.path().join("raced.m4dproj")).unwrap();
        let (race_capture, race_opens) = frozen_capture("provisional.m4dproj", &frozen);
        let mut injected_race = false;
        assert!(matches!(
            install_initial_provisional_autosave_package(
                &race_destination,
                race_capture,
                ProjectStoreLimits::default(),
                || {
                    if !injected_race
                        && fs::read_dir(race_parent.path()).unwrap().any(|entry| {
                            let stage = entry.unwrap().path();
                            stage
                                .file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| name.starts_with(".mirante4d-project-stage-"))
                                && stage.join("refs/autosave-head").is_file()
                        })
                    {
                        fs::create_dir(race_destination.as_path()).unwrap();
                        fs::write(race_destination.as_path().join("marker"), b"racer").unwrap();
                        injected_race = true;
                    }
                    false
                },
            ),
            Err(ProjectStoreFault::DestinationExists)
        ));
        assert!(injected_race);
        assert!(race_opens.load(Ordering::SeqCst) > 0);
        assert_eq!(
            fs::read(race_destination.as_path().join("marker")).unwrap(),
            b"racer"
        );
        assert_eq!(fs::read_dir(race_parent.path()).unwrap().count(), 1);

        let object = frozen.reachable_objects()[0];
        let path = object_path(retry_destination.as_path(), object.digest());
        let mut corrupt = fs::read(&path).unwrap();
        corrupt[0] ^= 1;
        fs::write(&path, corrupt).unwrap();
        let (corrupt_retry, corrupt_opens) = frozen_capture("provisional.m4dproj", &frozen);
        assert!(matches!(
            install_initial_provisional_autosave_package(
                &retry_destination,
                corrupt_retry,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. } | ProjectStoreFault::DigestMismatch)
        ));
        assert_eq!(corrupt_opens.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn provisional_advance_rejects_invalid_captures_and_allows_lower_revision() {
        let frozen = frozen_generation("provisional.m4dproj", PROVISIONAL_AUTOSAVE);
        let first_id = ProjectGenerationId::parse(PROVISIONAL_AUTOSAVE).unwrap();
        let parent = TestDirectory::new("provisional-advance-validation");
        let destination = ProjectStorePath::new(parent.path().join("validation.m4dproj")).unwrap();
        let (initial, _) = frozen_capture("provisional.m4dproj", &frozen);
        let installed = install_initial_provisional_autosave_package(
            &destination,
            initial,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let (root, leases, _) = installed.into_parts();
        let initial_head = fs::read(destination.as_path().join("refs/autosave-head")).unwrap();

        let cases = [
            (
                projection_with_revision(&frozen, 1, 1),
                None,
                None,
                "stale parent",
            ),
            (
                projection_with_revision(&frozen, 1, 1),
                Some(first_id),
                Some(first_id),
                "unexpected manual base",
            ),
            (
                projection_with_revision(&frozen, 0, 0),
                Some(first_id),
                None,
                "regressed high water",
            ),
        ];
        for (projection, expected_parent, base, label) in cases {
            let (capture, opens) = frozen_capture_with_facts(
                "provisional.m4dproj",
                &frozen,
                projection,
                expected_parent,
                base,
            );
            let result = publish_provisional_autosave_generation(
                &root,
                &leases,
                capture,
                ProjectStoreLimits::default(),
                || false,
            );
            assert!(result.is_err(), "{label} was accepted");
            assert_eq!(opens.load(Ordering::SeqCst), 0, "{label} read a source");
            assert_eq!(
                fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
                initial_head,
                "{label} changed the head"
            );
            assert!(
                !destination
                    .as_path()
                    .join("refs/autosave-recovery")
                    .exists()
            );
        }

        let source_state = frozen.projection().state();
        let foreign_project = ProjectId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap();
        let foreign_state = ProjectState::new(
            foreign_project,
            source_state.dataset().clone(),
            source_state.view().clone(),
            source_state.channel_presets().to_vec(),
            source_state.artifacts().to_vec(),
        )
        .unwrap();
        let foreign_projection = ProjectGenerationProjection::new(
            ProjectRevisionId::new(foreign_project, 1),
            ProjectRevisionHighWater::new(foreign_project, 1),
            foreign_state,
        )
        .unwrap();
        let foreign_scientific_id = ScientificContentId::parse(&format!(
            "{}{}",
            ScientificContentId::PREFIX,
            "99".repeat(32)
        ))
        .unwrap();
        let foreign_dataset = DatasetReference::new(
            foreign_scientific_id,
            source_state.dataset().package_id().cloned(),
            source_state.dataset().release_id().cloned(),
            source_state.dataset().locator_hint().cloned(),
        );
        let scientific_state = ProjectState::new(
            source_state.project_id(),
            foreign_dataset,
            source_state.view().clone(),
            source_state.channel_presets().to_vec(),
            source_state.artifacts().to_vec(),
        )
        .unwrap();
        let scientific_projection = ProjectGenerationProjection::new(
            ProjectRevisionId::new(source_state.project_id(), 1),
            ProjectRevisionHighWater::new(source_state.project_id(), 1),
            scientific_state,
        )
        .unwrap();
        let (fork_capture, fork_opens) = frozen_capture_with_all_facts(
            "provisional.m4dproj",
            &frozen,
            frozen.projection().clone(),
            Some(first_id),
            None,
            Some((
                ProjectId::parse(STALE_PROJECT).unwrap(),
                ProjectGenerationId::parse(STALE_GENERATION).unwrap(),
            )),
        );
        let (project_capture, project_opens) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            foreign_projection,
            Some(first_id),
            None,
        );
        let (scientific_capture, scientific_opens) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            scientific_projection,
            Some(first_id),
            None,
        );
        for (label, capture, opens) in [
            ("fork provenance", fork_capture, fork_opens),
            ("project identity", project_capture, project_opens),
            ("scientific identity", scientific_capture, scientific_opens),
        ] {
            assert!(
                publish_provisional_autosave_generation(
                    &root,
                    &leases,
                    capture,
                    ProjectStoreLimits::default(),
                    || false,
                )
                .is_err(),
                "{label} was accepted"
            );
            assert_eq!(opens.load(Ordering::SeqCst), 0, "{label} read a source");
            assert_eq!(
                fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
                initial_head,
                "{label} changed the head"
            );
            assert!(
                !destination
                    .as_path()
                    .join("refs/autosave-recovery")
                    .exists()
            );
        }

        let lower_revision = projection_with_revision(&frozen, 0, 1);
        let (capture, _) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            lower_revision,
            Some(first_id),
            None,
        );
        let receipt = publish_provisional_autosave_generation(
            &root,
            &leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.captured_revision().sequence(), 0);
        assert_eq!(receipt.captured_revision_high_water().sequence(), 1);
        assert_eq!(receipt.previous_generation_id(), Some(first_id));
        assert_eq!(receipt.autosave_base_generation_id(), None);
        let head = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
        )
        .unwrap();
        assert_eq!(head.current(), receipt.new_generation_id());
        assert_eq!(head.previous(), Some(first_id));
        assert_eq!(head.base(), None);
        assert!(!destination.as_path().join("refs/head").exists());
    }

    #[test]
    fn provisional_advance_retries_recovery_ahead_and_suspends_on_head_uncertainty() {
        let frozen = frozen_generation("provisional.m4dproj", PROVISIONAL_AUTOSAVE);
        let first_id = ProjectGenerationId::parse(PROVISIONAL_AUTOSAVE).unwrap();
        let next_revision = frozen.projection().revision().sequence() + 1;
        let next_high_water = frozen.projection().revision_high_water().sequence() + 1;
        let projection = projection_with_revision(&frozen, next_revision, next_high_water);
        let expected = GenerationDocument::build_from_projection(
            projection.clone(),
            Some(first_id),
            None,
            None,
            GenerationKind::Autosave,
            1,
            frozen.bindings().clone(),
            frozen.reachable_objects().to_vec(),
            ProjectStoreLimits::default(),
        )
        .unwrap()
        .encode(ProjectStoreLimits::default())
        .unwrap()
        .id();

        let parent = TestDirectory::new("provisional-advance");
        let destination = ProjectStorePath::new(parent.path().join("advance.m4dproj")).unwrap();
        let (initial, _) = frozen_capture("provisional.m4dproj", &frozen);
        let installed = install_initial_provisional_autosave_package(
            &destination,
            initial,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let (root, leases, _) = installed.into_parts();
        let (cancelled_before_recovery, _) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            projection.clone(),
            Some(first_id),
            None,
        );
        assert!(matches!(
            publish_provisional_autosave_generation(
                &root,
                &leases,
                cancelled_before_recovery,
                ProjectStoreLimits::default(),
                || generation_path(destination.as_path(), expected).is_file()
                    && !destination
                        .as_path()
                        .join("refs/autosave-recovery")
                        .exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert!(
            !destination
                .as_path()
                .join("refs/autosave-recovery")
                .exists()
        );
        let unchanged = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
        )
        .unwrap();
        assert_eq!(unchanged.current(), first_id);

        let (cancelled_after_recovery, _) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            projection.clone(),
            Some(first_id),
            None,
        );
        assert!(matches!(
            publish_provisional_autosave_generation(
                &root,
                &leases,
                cancelled_after_recovery,
                ProjectStoreLimits::default(),
                || destination
                    .as_path()
                    .join("refs/autosave-recovery")
                    .exists()
                    && RefRecord::decode(
                        RefKind::AutosaveHead,
                        &fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
                    )
                    .is_ok_and(|head| head.current() == first_id),
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        let recovery = RefRecord::decode(
            RefKind::AutosaveRecovery,
            &fs::read(destination.as_path().join("refs/autosave-recovery")).unwrap(),
        )
        .unwrap();
        assert_eq!(recovery.current(), first_id);
        let old_head = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
        )
        .unwrap();
        assert_eq!(old_head.current(), first_id);
        assert_eq!(old_head.previous(), None);
        assert_eq!(old_head.base(), None);
        assert!(leases.confirm_writer(&root).unwrap());

        let (retry, _) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            projection.clone(),
            Some(first_id),
            None,
        );
        let receipt = publish_provisional_autosave_generation(
            &root,
            &leases,
            retry,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.new_generation_id(), expected);
        assert_eq!(receipt.previous_generation_id(), Some(first_id));
        assert_eq!(receipt.autosave_base_generation_id(), None);
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        let head = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(destination.as_path().join("refs/autosave-head")).unwrap(),
        )
        .unwrap();
        assert_eq!(head.current(), expected);
        assert_eq!(head.previous(), Some(first_id));
        assert_eq!(head.base(), None);
        assert!(!destination.as_path().join("refs/head").exists());
        assert!(!destination.as_path().join("refs/recovery").exists());

        let indeterminate_parent = TestDirectory::new("provisional-head-indeterminate");
        let indeterminate_destination =
            ProjectStorePath::new(indeterminate_parent.path().join("indeterminate.m4dproj"))
                .unwrap();
        let (indeterminate_initial, _) = frozen_capture("provisional.m4dproj", &frozen);
        let indeterminate = install_initial_provisional_autosave_package(
            &indeterminate_destination,
            indeterminate_initial,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let (indeterminate_root, indeterminate_leases, _) = indeterminate.into_parts();
        indeterminate_root.fail_ref_commit_directory_sync_at(
            indeterminate_root.ref_commit_directory_sync_attempts() + 2,
        );
        let (capture, _) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            projection.clone(),
            Some(first_id),
            None,
        );
        assert!(matches!(
            publish_provisional_autosave_generation(
                &indeterminate_root,
                &indeterminate_leases,
                capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::CommitIndeterminate)
        ));
        assert!(matches!(
            indeterminate_leases.confirm_writer(&indeterminate_root),
            Err(LeaseError::Indeterminate)
        ));
        let visible = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(
                indeterminate_destination
                    .as_path()
                    .join("refs/autosave-head"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(visible.current(), expected);
        assert_eq!(visible.previous(), Some(first_id));
        assert_eq!(visible.base(), None);

        drop(indeterminate_leases);
        drop(indeterminate_root);
        let retry_root = LocalStoreRoot::open(indeterminate_destination.as_path()).unwrap();
        let retry_leases =
            ProjectStoreLeases::acquire(&retry_root, ProjectOpenMode::PreferWritable).unwrap();
        let (fresh_retry, fresh_retry_opens) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            projection.clone(),
            Some(first_id),
            None,
        );
        let recovered = publish_provisional_autosave_generation(
            &retry_root,
            &retry_leases,
            fresh_retry,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(fresh_retry_opens.load(Ordering::SeqCst), 0);
        assert_eq!(recovered.new_generation_id(), expected);
        assert_eq!(recovered.previous_generation_id(), Some(first_id));
        assert_eq!(recovered.autosave_base_generation_id(), None);
        assert_eq!(recovered.published_objects(), 0);
        assert_eq!(recovered.published_bytes(), 0);

        let (second_retry, second_retry_opens) = frozen_capture_with_facts(
            "provisional.m4dproj",
            &frozen,
            projection,
            Some(first_id),
            None,
        );
        let idempotent = publish_provisional_autosave_generation(
            &retry_root,
            &retry_leases,
            second_retry,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(second_retry_opens.load(Ordering::SeqCst), 0);
        assert_eq!(idempotent, recovered);
    }

    #[test]
    fn first_manual_commit_rejects_stale_or_corrupt_head_before_publication() {
        let (_, frozen, _) = frozen_stale_manual();

        let read_only_directory = prepared_frozen_root("initial-read-only");
        let read_only_root = LocalStoreRoot::open(read_only_directory.path()).unwrap();
        let read_only_leases =
            ProjectStoreLeases::acquire(&read_only_root, ProjectOpenMode::ReadOnly).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &read_only_root,
                &read_only_leases,
                rejected_frozen_capture(&frozen),
                InitialPackageMode::Create,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::ReadOnly)
        ));
        assert!(!read_only_directory.path().join("objects").exists());
        assert!(!read_only_directory.path().join("generations").exists());

        let stale_directory = prepared_frozen_root("initial-stale");
        fs::create_dir(stale_directory.path().join("refs")).unwrap();
        fs::write(
            stale_directory.path().join("refs/head"),
            fixture_extract("stale.m4dproj/refs/head"),
        )
        .unwrap();
        let stale_root = LocalStoreRoot::open(stale_directory.path()).unwrap();
        let stale_leases =
            ProjectStoreLeases::acquire(&stale_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &stale_root,
                &stale_leases,
                rejected_frozen_capture(&frozen),
                InitialPackageMode::Create,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::StaleParent)
        ));
        assert!(!stale_directory.path().join("objects").exists());
        assert!(!stale_directory.path().join("generations").exists());

        let corrupt_directory = prepared_frozen_root("initial-corrupt");
        fs::create_dir(corrupt_directory.path().join("refs")).unwrap();
        fs::write(
            corrupt_directory.path().join("refs/head"),
            fixture_extract("stale.m4dproj/refs/autosave-head"),
        )
        .unwrap();
        let corrupt_root = LocalStoreRoot::open(corrupt_directory.path()).unwrap();
        let corrupt_leases =
            ProjectStoreLeases::acquire(&corrupt_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &corrupt_root,
                &corrupt_leases,
                rejected_frozen_capture(&frozen),
                InitialPackageMode::Create,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert!(!corrupt_directory.path().join("objects").exists());
        assert!(!corrupt_directory.path().join("generations").exists());

        let pinned_directory = prepared_frozen_root("initial-pinned");
        fs::create_dir_all(pinned_directory.path().join("refs/pins")).unwrap();
        fs::write(
            pinned_directory.path().join("refs/pins/checkpoint-a"),
            fixture_extract("recoverable.m4dproj/refs/pins/checkpoint-a"),
        )
        .unwrap();
        let pinned_root = LocalStoreRoot::open(pinned_directory.path()).unwrap();
        let pinned_leases =
            ProjectStoreLeases::acquire(&pinned_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &pinned_root,
                &pinned_leases,
                rejected_frozen_capture(&frozen),
                InitialPackageMode::Create,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert!(!pinned_directory.path().join("objects").exists());
        assert!(!pinned_directory.path().join("generations").exists());
    }

    #[test]
    fn first_manual_commit_cancellation_after_generation_leaves_no_ref() {
        let directory = prepared_frozen_root("initial-cancel");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let (frozen_id, frozen, _) = frozen_stale_manual();
        let (capture, _) = frozen_capture("stale.m4dproj", &frozen);
        let generation = generation_path(directory.path(), frozen_id);

        assert!(matches!(
            publish_initial_manual_generation(
                &root,
                &leases,
                capture,
                InitialPackageMode::Create,
                ProjectStoreLimits::default(),
                || generation.exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert!(generation.exists());
        assert!(!directory.path().join("refs/head").exists());
        assert!(!directory.path().join("refs/recovery").exists());
    }

    #[test]
    fn established_manual_commit_matches_the_frozen_recovery_and_head() {
        let directory =
            prepared_frozen_root_from_store("established-success", "recoverable.m4dproj");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let g1 = frozen_generation("recoverable.m4dproj", RECOVERABLE_G1);
        let g2 = frozen_generation("recoverable.m4dproj", RECOVERABLE_G2);

        let (initial, _) = frozen_capture("recoverable.m4dproj", &g1);
        publish_initial_manual_generation(
            &root,
            &leases,
            initial,
            InitialPackageMode::Create,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let (capture, opens) = frozen_capture("recoverable.m4dproj", &g2);
        let receipt = publish_established_manual_generation(
            &root,
            &leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();

        let g1_id = ProjectGenerationId::parse(RECOVERABLE_G1).unwrap();
        let g2_id = ProjectGenerationId::parse(RECOVERABLE_G2).unwrap();
        assert_eq!(receipt.new_generation_id(), g2_id);
        assert_eq!(receipt.current_generation_id(), g2_id);
        assert_eq!(receipt.previous_generation_id(), Some(g1_id));
        assert_eq!(receipt.published_objects(), 3);
        assert_eq!(
            fs::read(directory.path().join("refs/recovery")).unwrap(),
            fixture_extract("recoverable.m4dproj/refs/recovery")
        );
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            fixture_extract("recoverable.m4dproj/refs/head")
        );
        assert!(generation_path(directory.path(), g1_id).is_file());
        assert!(generation_path(directory.path(), g2_id).is_file());
        assert!(!directory.path().join("refs/autosave-head").exists());
        assert!(!directory.path().join("refs/autosave-recovery").exists());
        assert!(opens.load(Ordering::SeqCst) >= g2.projection().state().artifacts().len());
    }

    #[test]
    fn established_manual_commit_uses_autosave_sequence_and_preserves_other_refs() {
        let directory =
            extracted_frozen_root("established-autosave-sequence", "recoverable.m4dproj");
        let autosave_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();
        let manual_head = RefRecord::decode(
            RefKind::ManualHead,
            &fs::read(directory.path().join("refs/head")).unwrap(),
        )
        .unwrap();
        let manual_recovery_ahead = RefRecord::new(
            RefKind::ManualRecovery,
            manual_head.project_id(),
            manual_head.current(),
            None,
            None,
        )
        .unwrap()
        .encode();
        fs::write(
            directory.path().join("refs/recovery"),
            manual_recovery_ahead,
        )
        .unwrap();
        let autosave_head_record =
            RefRecord::decode(RefKind::AutosaveHead, &autosave_head).unwrap();
        let autosave_recovery = RefRecord::new(
            RefKind::AutosaveRecovery,
            autosave_head_record.project_id(),
            autosave_head_record.current(),
            None,
            None,
        )
        .unwrap()
        .encode();
        fs::write(
            directory.path().join("refs/autosave-recovery"),
            autosave_recovery,
        )
        .unwrap();
        let pin = fs::read(directory.path().join("refs/pins/checkpoint-a")).unwrap();
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let orphan = frozen_generation("recoverable.m4dproj", RECOVERABLE_ORPHAN);
        assert_eq!(orphan.generation_sequence(), 4);
        let (capture, _) = frozen_capture("recoverable.m4dproj", &orphan);

        let receipt = publish_established_manual_generation(
            &root,
            &leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let project_id = orphan.projection().state().project_id();
        let old = ProjectGenerationId::parse(RECOVERABLE_G2).unwrap();
        let new = ProjectGenerationId::parse(RECOVERABLE_ORPHAN).unwrap();
        let expected_recovery =
            RefRecord::new(RefKind::ManualRecovery, project_id, old, None, None)
                .unwrap()
                .encode();
        let expected_head = RefRecord::new(RefKind::ManualHead, project_id, new, Some(old), None)
            .unwrap()
            .encode();

        assert_eq!(receipt.new_generation_id(), new);
        assert_eq!(receipt.previous_generation_id(), Some(old));
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        assert_eq!(
            fs::read(directory.path().join("refs/recovery")).unwrap(),
            expected_recovery
        );
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            expected_head
        );
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            autosave_head
        );
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-recovery")).unwrap(),
            autosave_recovery
        );
        assert_eq!(
            fs::read(directory.path().join("refs/pins/checkpoint-a")).unwrap(),
            pin
        );
    }

    #[test]
    fn established_manual_commit_rejects_invalid_state_and_capacity_without_ref_change() {
        let directory =
            prepared_frozen_root_from_store("established-reject", "recoverable.m4dproj");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let g1 = frozen_generation("recoverable.m4dproj", RECOVERABLE_G1);
        let g2 = frozen_generation("recoverable.m4dproj", RECOVERABLE_G2);
        let (initial, _) = frozen_capture("recoverable.m4dproj", &g1);
        publish_initial_manual_generation(
            &root,
            &leases,
            initial,
            InitialPackageMode::Create,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let initial_head = fs::read(directory.path().join("refs/head")).unwrap();

        let (stale_capture, stale_opens) =
            frozen_capture_with_parent("recoverable.m4dproj", &g2, None);
        assert!(matches!(
            publish_established_manual_generation(
                &root,
                &leases,
                stale_capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::StaleParent)
        ));
        assert_eq!(stale_opens.load(Ordering::SeqCst), 0);
        assert!(
            !generation_path(
                directory.path(),
                ProjectGenerationId::parse(RECOVERABLE_G2).unwrap()
            )
            .exists()
        );
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            initial_head
        );

        let corrupt_directory =
            extracted_frozen_root("established-unrelated", "recoverable.m4dproj");
        let corrupt_root = LocalStoreRoot::open(corrupt_directory.path()).unwrap();
        let corrupt_leases =
            ProjectStoreLeases::acquire(&corrupt_root, ProjectOpenMode::PreferWritable).unwrap();
        let orphan = frozen_generation("recoverable.m4dproj", RECOVERABLE_ORPHAN);
        let unrelated = RefRecord::new(
            RefKind::ManualRecovery,
            orphan.projection().state().project_id(),
            ProjectGenerationId::parse(RECOVERABLE_ORPHAN).unwrap(),
            None,
            None,
        )
        .unwrap();
        fs::write(
            corrupt_directory.path().join("refs/recovery"),
            unrelated.encode(),
        )
        .unwrap();
        let original_head = fixture_extract("recoverable.m4dproj/refs/head");
        let (capture, opens) = frozen_capture("recoverable.m4dproj", &orphan);
        assert!(matches!(
            publish_established_manual_generation(
                &corrupt_root,
                &corrupt_leases,
                capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "recovery_pair"
            })
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(corrupt_directory.path().join("refs/head")).unwrap(),
            original_head
        );

        let missing_directory =
            extracted_frozen_root("established-missing-previous", "recoverable.m4dproj");
        fs::remove_file(generation_path(
            missing_directory.path(),
            ProjectGenerationId::parse(RECOVERABLE_G1).unwrap(),
        ))
        .unwrap();
        let missing_root = LocalStoreRoot::open(missing_directory.path()).unwrap();
        let missing_leases =
            ProjectStoreLeases::acquire(&missing_root, ProjectOpenMode::PreferWritable).unwrap();
        let (capture, opens) = frozen_capture("recoverable.m4dproj", &orphan);
        assert!(matches!(
            publish_established_manual_generation(
                &missing_root,
                &missing_leases,
                capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);

        let staging_directory =
            extracted_frozen_root("established-staging-preflight", "recoverable.m4dproj");
        let staging_root = LocalStoreRoot::open(staging_directory.path()).unwrap();
        let staging_leases =
            ProjectStoreLeases::acquire(&staging_root, ProjectOpenMode::PreferWritable).unwrap();
        let private_stage = staging_directory.path().join("staging/stale-transaction");
        fs::create_dir_all(&private_stage).unwrap();
        symlink(
            staging_directory.path().join("project.json"),
            private_stage.join("invalid-link"),
        )
        .unwrap();
        let (capture, opens) = frozen_capture("recoverable.m4dproj", &orphan);
        assert!(matches!(
            publish_established_manual_generation(
                &staging_root,
                &staging_leases,
                capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "store_inventory"
            })
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);

        let capacity_directory =
            prepared_frozen_root_from_store("established-capacity", "recoverable.m4dproj");
        let capacity_root = LocalStoreRoot::open(capacity_directory.path()).unwrap();
        let capacity_leases =
            ProjectStoreLeases::acquire(&capacity_root, ProjectOpenMode::PreferWritable).unwrap();
        let (initial, _) = frozen_capture("recoverable.m4dproj", &g1);
        publish_initial_manual_generation(
            &capacity_root,
            &capacity_leases,
            initial,
            InitialPackageMode::Create,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let old_head = fs::read(capacity_directory.path().join("refs/head")).unwrap();
        let capacity_limits = ProjectStoreLimits {
            physical_store_entries_max: 1,
            ..ProjectStoreLimits::default()
        };
        let (capture, _) = frozen_capture("recoverable.m4dproj", &g2);
        assert!(matches!(
            publish_established_manual_generation(
                &capacity_root,
                &capacity_leases,
                capture,
                capacity_limits,
                || false,
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "store_inventory"
            })
        ));
        assert_eq!(
            fs::read(capacity_directory.path().join("refs/head")).unwrap(),
            old_head
        );
        assert!(!capacity_directory.path().join("refs/recovery").exists());
    }

    #[test]
    fn established_manual_commit_retries_after_cancelled_recovery_ahead() {
        let directory = prepared_frozen_root_from_store("established-retry", "recoverable.m4dproj");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let g1 = frozen_generation("recoverable.m4dproj", RECOVERABLE_G1);
        let g2 = frozen_generation("recoverable.m4dproj", RECOVERABLE_G2);
        let project_id = g1.projection().state().project_id();
        let g1_id = ProjectGenerationId::parse(RECOVERABLE_G1).unwrap();
        let (initial, _) = frozen_capture("recoverable.m4dproj", &g1);
        publish_initial_manual_generation(
            &root,
            &leases,
            initial,
            InitialPackageMode::Create,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();

        let recovery_path = directory.path().join("refs/recovery");
        let (capture, _) = frozen_capture("recoverable.m4dproj", &g2);
        assert!(matches!(
            publish_established_manual_generation(
                &root,
                &leases,
                capture,
                ProjectStoreLimits::default(),
                || recovery_path.exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        let old_head = RefRecord::new(RefKind::ManualHead, project_id, g1_id, None, None)
            .unwrap()
            .encode();
        let recovery_ahead = RefRecord::new(RefKind::ManualRecovery, project_id, g1_id, None, None)
            .unwrap()
            .encode();
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            old_head
        );
        assert_eq!(fs::read(&recovery_path).unwrap(), recovery_ahead);

        let (retry, _) = frozen_capture("recoverable.m4dproj", &g2);
        let receipt = publish_established_manual_generation(
            &root,
            &leases,
            retry,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            fixture_extract("recoverable.m4dproj/refs/head")
        );
        assert_eq!(
            fs::read(&recovery_path).unwrap(),
            fixture_extract("recoverable.m4dproj/refs/recovery")
        );
    }

    #[test]
    fn established_manual_sync_failures_preserve_retry_and_head_indeterminacy() {
        let manual = frozen_generation("stale.m4dproj", STALE_GENERATION);
        let project_id = manual.projection().state().project_id();
        let manual_id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let expected_generation = GenerationDocument::build_from_projection(
            manual.projection().clone(),
            Some(manual_id),
            None,
            manual.forked_from(),
            GenerationKind::Manual,
            2,
            manual.bindings().clone(),
            manual.reachable_objects().to_vec(),
            ProjectStoreLimits::default(),
        )
        .unwrap()
        .encode(ProjectStoreLimits::default())
        .unwrap()
        .id();
        let expected_recovery =
            RefRecord::new(RefKind::ManualRecovery, project_id, manual_id, None, None)
                .unwrap()
                .encode();
        let expected_head = RefRecord::new(
            RefKind::ManualHead,
            project_id,
            expected_generation,
            Some(manual_id),
            None,
        )
        .unwrap()
        .encode();

        for failed_sync in [0, 1] {
            let directory = extracted_frozen_root(
                &format!("established-recovery-sync-{failed_sync}"),
                "stale.m4dproj",
            );
            let root = LocalStoreRoot::open(directory.path()).unwrap();
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let old_head = fs::read(directory.path().join("refs/head")).unwrap();
            let autosave_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();
            root.fail_ref_commit_directory_sync_at(failed_sync);
            let (capture, _) = frozen_capture_with_facts(
                "stale.m4dproj",
                &manual,
                manual.projection().clone(),
                Some(manual_id),
                None,
            );
            assert!(matches!(
                publish_established_manual_generation(
                    &root,
                    &leases,
                    capture,
                    ProjectStoreLimits::default(),
                    || false,
                ),
                Err(ProjectStoreFault::Corruption {
                    stage: "manual_recovery_durability"
                })
            ));
            assert_eq!(root.ref_commit_directory_sync_attempts(), 2);
            assert_eq!(
                fs::read(directory.path().join("refs/head")).unwrap(),
                old_head
            );
            assert_eq!(
                fs::read(directory.path().join("refs/recovery")).unwrap(),
                expected_recovery
            );
            assert!(leases.confirm_writer(&root).unwrap());

            let (retry, _) = frozen_capture_with_facts(
                "stale.m4dproj",
                &manual,
                manual.projection().clone(),
                Some(manual_id),
                None,
            );
            let receipt = publish_established_manual_generation(
                &root,
                &leases,
                retry,
                ProjectStoreLimits::default(),
                || false,
            )
            .unwrap();
            assert_eq!(receipt.new_generation_id(), expected_generation);
            assert_eq!(receipt.published_objects(), 0);
            assert_eq!(receipt.published_bytes(), 0);
            assert_eq!(
                fs::read(directory.path().join("refs/recovery")).unwrap(),
                expected_recovery
            );
            assert_eq!(
                fs::read(directory.path().join("refs/head")).unwrap(),
                expected_head
            );
            assert_eq!(
                fs::read(directory.path().join("refs/autosave-head")).unwrap(),
                autosave_head
            );
        }

        for failed_sync in [2, 3] {
            let directory = extracted_frozen_root(
                &format!("established-head-sync-{failed_sync}"),
                "stale.m4dproj",
            );
            let root = LocalStoreRoot::open(directory.path()).unwrap();
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let old_head = fs::read(directory.path().join("refs/head")).unwrap();
            let autosave_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();
            root.fail_ref_commit_directory_sync_at(failed_sync);

            let (capture, _) = frozen_capture_with_facts(
                "stale.m4dproj",
                &manual,
                manual.projection().clone(),
                Some(manual_id),
                None,
            );
            assert!(matches!(
                publish_established_manual_generation(
                    &root,
                    &leases,
                    capture,
                    ProjectStoreLimits::default(),
                    || fs::read(directory.path().join("refs/head"))
                        .is_ok_and(|head| head != old_head),
                ),
                Err(ProjectStoreFault::CommitIndeterminate)
            ));
            assert_eq!(root.ref_commit_directory_sync_attempts(), 4);
            assert_eq!(
                fs::read(directory.path().join("refs/recovery")).unwrap(),
                expected_recovery
            );
            assert_eq!(
                fs::read(directory.path().join("refs/head")).unwrap(),
                expected_head
            );
            assert_eq!(
                fs::read(directory.path().join("refs/autosave-head")).unwrap(),
                autosave_head
            );
            assert!(matches!(
                leases.confirm_writer(&root),
                Err(LeaseError::Indeterminate)
            ));
        }
    }

    #[test]
    fn first_established_autosave_matches_the_frozen_head() {
        let directory = prepared_frozen_root("first-established-autosave");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let (_, manual, _) = frozen_stale_manual();
        let autosave = frozen_generation("stale.m4dproj", STALE_AUTOSAVE);
        let manual_id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let autosave_id = ProjectGenerationId::parse(STALE_AUTOSAVE).unwrap();

        let (initial, _) = frozen_capture("stale.m4dproj", &manual);
        publish_initial_manual_generation(
            &root,
            &leases,
            initial,
            InitialPackageMode::Create,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        let manual_head = fs::read(directory.path().join("refs/head")).unwrap();
        let (capture, opens) = frozen_capture("stale.m4dproj", &autosave);
        let receipt = publish_established_autosave_generation(
            &root,
            &leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();

        assert_eq!(receipt.new_generation_id(), autosave_id);
        assert_eq!(receipt.previous_generation_id(), None);
        assert_eq!(receipt.autosave_base_generation_id(), Some(manual_id));
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(generation_path(directory.path(), autosave_id)).unwrap(),
            fixture_extract(&fixture_generation_member("stale.m4dproj", autosave_id))
        );
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            fixture_extract("stale.m4dproj/refs/autosave-head")
        );
        assert!(!directory.path().join("refs/autosave-recovery").exists());
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            manual_head
        );
    }

    #[test]
    fn established_autosave_advances_clean_and_divergent_lanes() {
        let directory = extracted_frozen_root("established-autosave-clean", "recoverable.m4dproj");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let a1 = ProjectGenerationId::parse(RECOVERABLE_A1).unwrap();
        let a2 = ProjectGenerationId::parse(RECOVERABLE_A2).unwrap();
        let manual = ProjectGenerationId::parse(RECOVERABLE_G2).unwrap();
        let a2_document = frozen_generation("recoverable.m4dproj", RECOVERABLE_A2);
        let a1_head = RefRecord::new(
            RefKind::AutosaveHead,
            a2_document.projection().state().project_id(),
            a1,
            None,
            Some(manual),
        )
        .unwrap()
        .encode();
        fs::write(directory.path().join("refs/autosave-head"), a1_head).unwrap();
        fs::remove_file(directory.path().join("refs/autosave-recovery")).unwrap();
        let manual_head = fs::read(directory.path().join("refs/head")).unwrap();
        let manual_recovery = fs::read(directory.path().join("refs/recovery")).unwrap();
        let pin = fs::read(directory.path().join("refs/pins/checkpoint-a")).unwrap();

        let (capture, _opens) = frozen_capture("recoverable.m4dproj", &a2_document);
        let receipt = publish_established_autosave_generation(
            &root,
            &leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.new_generation_id(), a2);
        assert_eq!(receipt.previous_generation_id(), Some(a1));
        assert_eq!(receipt.autosave_base_generation_id(), Some(manual));
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            fixture_extract("recoverable.m4dproj/refs/autosave-head")
        );
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-recovery")).unwrap(),
            fixture_extract("recoverable.m4dproj/refs/autosave-recovery")
        );
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            manual_head
        );
        assert_eq!(
            fs::read(directory.path().join("refs/recovery")).unwrap(),
            manual_recovery
        );
        assert_eq!(
            fs::read(directory.path().join("refs/pins/checkpoint-a")).unwrap(),
            pin
        );

        let divergent_directory =
            extracted_frozen_root("established-autosave-divergent", "divergent.m4dproj");
        let divergent_root = LocalStoreRoot::open(divergent_directory.path()).unwrap();
        let divergent_leases =
            ProjectStoreLeases::acquire(&divergent_root, ProjectOpenMode::PreferWritable).unwrap();
        let old_autosave = frozen_generation("divergent.m4dproj", DIVERGENT_AUTOSAVE);
        let old_autosave_id = ProjectGenerationId::parse(DIVERGENT_AUTOSAVE).unwrap();
        let current_manual = ProjectGenerationId::parse(DIVERGENT_MANUAL).unwrap();
        let manual_head = fs::read(divergent_directory.path().join("refs/head")).unwrap();
        let manual_recovery = fs::read(divergent_directory.path().join("refs/recovery")).unwrap();
        let projection = projection_with_revision(&old_autosave, 3, 4);
        let (capture, _opens) = frozen_capture_with_facts(
            "divergent.m4dproj",
            &old_autosave,
            projection,
            Some(old_autosave_id),
            Some(current_manual),
        );
        let receipt = publish_established_autosave_generation(
            &divergent_root,
            &divergent_leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.previous_generation_id(), Some(old_autosave_id));
        assert_eq!(receipt.autosave_base_generation_id(), Some(current_manual));
        assert_eq!(receipt.captured_revision().sequence(), 3);
        assert_eq!(receipt.captured_revision_high_water().sequence(), 4);
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        let generation_bytes = fs::read(generation_path(
            divergent_directory.path(),
            receipt.new_generation_id(),
        ))
        .unwrap();
        let generation = GenerationDocument::decode(
            receipt.new_generation_id(),
            old_autosave.projection().state().project_id(),
            &generation_bytes,
            ProjectStoreLimits::default(),
        )
        .unwrap();
        assert_eq!(generation.kind(), GenerationKind::Autosave);
        assert_eq!(generation.generation_sequence(), 4);
        assert_eq!(generation.parent_generation_id(), Some(old_autosave_id));
        assert_eq!(generation.base_manual_generation_id(), Some(current_manual));
        assert_eq!(generation.projection().revision().sequence(), 3);
        assert_eq!(generation.projection().revision_high_water().sequence(), 4);
        let head = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(divergent_directory.path().join("refs/autosave-head")).unwrap(),
        )
        .unwrap();
        assert_eq!(head.current(), receipt.new_generation_id());
        assert_eq!(head.previous(), Some(old_autosave_id));
        assert_eq!(head.base(), Some(current_manual));
        let recovery = RefRecord::decode(
            RefKind::AutosaveRecovery,
            &fs::read(divergent_directory.path().join("refs/autosave-recovery")).unwrap(),
        )
        .unwrap();
        assert_eq!(recovery.current(), old_autosave_id);
        assert_eq!(
            fs::read(divergent_directory.path().join("refs/head")).unwrap(),
            manual_head
        );
        assert_eq!(
            fs::read(divergent_directory.path().join("refs/recovery")).unwrap(),
            manual_recovery
        );
    }

    #[test]
    fn established_autosave_rejects_invalid_state_and_capacity_without_ref_change() {
        let directory = extracted_frozen_root("established-autosave-reject", "stale.m4dproj");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let autosave = frozen_generation("stale.m4dproj", STALE_AUTOSAVE);
        let autosave_id = ProjectGenerationId::parse(STALE_AUTOSAVE).unwrap();
        let manual_id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let manual_head = fs::read(directory.path().join("refs/head")).unwrap();
        let autosave_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();

        let (wrong_parent, opens) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            autosave.projection().clone(),
            None,
            Some(manual_id),
        );
        assert!(matches!(
            publish_established_autosave_generation(
                &root,
                &leases,
                wrong_parent,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::StaleParent)
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);

        let (wrong_base, opens) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            autosave.projection().clone(),
            Some(autosave_id),
            None,
        );
        assert!(matches!(
            publish_established_autosave_generation(
                &root,
                &leases,
                wrong_base,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::StaleParent)
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);

        let (regressed, opens) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            projection_with_revision(&autosave, 4, 4),
            Some(autosave_id),
            Some(manual_id),
        );
        assert!(matches!(
            publish_established_autosave_generation(
                &root,
                &leases,
                regressed,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "revision_high_water_regression"
            })
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            manual_head
        );
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            autosave_head
        );
        assert!(!directory.path().join("refs/autosave-recovery").exists());

        let recovery_directory =
            extracted_frozen_root("established-autosave-unrelated", "stale.m4dproj");
        let recovery_root = LocalStoreRoot::open(recovery_directory.path()).unwrap();
        let recovery_leases =
            ProjectStoreLeases::acquire(&recovery_root, ProjectOpenMode::PreferWritable).unwrap();
        let unrelated = RefRecord::new(
            RefKind::AutosaveRecovery,
            autosave.projection().state().project_id(),
            manual_id,
            None,
            None,
        )
        .unwrap()
        .encode();
        fs::write(
            recovery_directory.path().join("refs/autosave-recovery"),
            unrelated,
        )
        .unwrap();
        let recovery_head = fs::read(recovery_directory.path().join("refs/autosave-head")).unwrap();
        let (capture, opens) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            autosave.projection().clone(),
            Some(autosave_id),
            Some(manual_id),
        );
        assert!(matches!(
            publish_established_autosave_generation(
                &recovery_root,
                &recovery_leases,
                capture,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "recovery_pair"
            })
        ));
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(recovery_directory.path().join("refs/autosave-head")).unwrap(),
            recovery_head
        );
        assert_eq!(
            fs::read(recovery_directory.path().join("refs/autosave-recovery")).unwrap(),
            unrelated
        );

        let capacity_directory =
            extracted_frozen_root("established-autosave-capacity", "stale.m4dproj");
        let capacity_root = LocalStoreRoot::open(capacity_directory.path()).unwrap();
        let capacity_leases =
            ProjectStoreLeases::acquire(&capacity_root, ProjectOpenMode::PreferWritable).unwrap();
        let old_manual_head = fs::read(capacity_directory.path().join("refs/head")).unwrap();
        let old_autosave_head =
            fs::read(capacity_directory.path().join("refs/autosave-head")).unwrap();
        let limits = ProjectStoreLimits {
            physical_store_entries_max: 1,
            ..ProjectStoreLimits::default()
        };
        let (capture, _) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            autosave.projection().clone(),
            Some(autosave_id),
            Some(manual_id),
        );
        assert!(matches!(
            publish_established_autosave_generation(
                &capacity_root,
                &capacity_leases,
                capture,
                limits,
                || false,
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "store_inventory"
            })
        ));
        assert_eq!(
            fs::read(capacity_directory.path().join("refs/head")).unwrap(),
            old_manual_head
        );
        assert_eq!(
            fs::read(capacity_directory.path().join("refs/autosave-head")).unwrap(),
            old_autosave_head
        );
        assert!(
            !capacity_directory
                .path()
                .join("refs/autosave-recovery")
                .exists()
        );
    }

    #[test]
    fn established_autosave_retries_after_cancelled_recovery_ahead() {
        let directory = extracted_frozen_root("established-autosave-retry", "stale.m4dproj");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let autosave = frozen_generation("stale.m4dproj", STALE_AUTOSAVE);
        let project_id = autosave.projection().state().project_id();
        let autosave_id = ProjectGenerationId::parse(STALE_AUTOSAVE).unwrap();
        let manual_id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let old_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();
        let manual_head = fs::read(directory.path().join("refs/head")).unwrap();
        let recovery_path = directory.path().join("refs/autosave-recovery");
        let expected_generation = GenerationDocument::build_from_projection(
            autosave.projection().clone(),
            Some(autosave_id),
            Some(manual_id),
            autosave.forked_from(),
            GenerationKind::Autosave,
            2,
            autosave.bindings().clone(),
            autosave.reachable_objects().to_vec(),
            ProjectStoreLimits::default(),
        )
        .unwrap()
        .encode(ProjectStoreLimits::default())
        .unwrap()
        .id();

        let (capture, _) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            autosave.projection().clone(),
            Some(autosave_id),
            Some(manual_id),
        );
        assert!(matches!(
            publish_established_autosave_generation(
                &root,
                &leases,
                capture,
                ProjectStoreLimits::default(),
                || recovery_path.exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        let recovery_ahead = RefRecord::new(
            RefKind::AutosaveRecovery,
            project_id,
            autosave_id,
            None,
            None,
        )
        .unwrap()
        .encode();
        assert_eq!(
            fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            old_head
        );
        assert_eq!(fs::read(&recovery_path).unwrap(), recovery_ahead);
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            manual_head
        );
        assert!(leases.confirm_writer(&root).unwrap());
        assert!(generation_path(directory.path(), expected_generation).exists());

        let (retry, _) = frozen_capture_with_facts(
            "stale.m4dproj",
            &autosave,
            autosave.projection().clone(),
            Some(autosave_id),
            Some(manual_id),
        );
        let receipt = publish_established_autosave_generation(
            &root,
            &leases,
            retry,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.new_generation_id(), expected_generation);
        assert_eq!(receipt.previous_generation_id(), Some(autosave_id));
        assert_eq!(receipt.autosave_base_generation_id(), Some(manual_id));
        assert_eq!(receipt.published_objects(), 0);
        assert_eq!(receipt.published_bytes(), 0);
        let head = RefRecord::decode(
            RefKind::AutosaveHead,
            &fs::read(directory.path().join("refs/autosave-head")).unwrap(),
        )
        .unwrap();
        assert_eq!(head.current(), receipt.new_generation_id());
        assert_eq!(head.previous(), Some(autosave_id));
        assert_eq!(head.base(), Some(manual_id));
        assert_eq!(fs::read(&recovery_path).unwrap(), recovery_ahead);
    }

    #[test]
    fn established_autosave_sync_failures_preserve_retry_and_head_indeterminacy() {
        let autosave = frozen_generation("stale.m4dproj", STALE_AUTOSAVE);
        let project_id = autosave.projection().state().project_id();
        let autosave_id = ProjectGenerationId::parse(STALE_AUTOSAVE).unwrap();
        let manual_id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let expected_recovery = RefRecord::new(
            RefKind::AutosaveRecovery,
            project_id,
            autosave_id,
            None,
            None,
        )
        .unwrap()
        .encode();

        for failed_sync in [0, 1] {
            let directory = extracted_frozen_root(
                &format!("established-autosave-recovery-sync-{failed_sync}"),
                "stale.m4dproj",
            );
            let root = LocalStoreRoot::open(directory.path()).unwrap();
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let old_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();
            let manual_head = fs::read(directory.path().join("refs/head")).unwrap();
            root.fail_ref_commit_directory_sync_at(failed_sync);
            let (capture, _) = frozen_capture_with_facts(
                "stale.m4dproj",
                &autosave,
                autosave.projection().clone(),
                Some(autosave_id),
                Some(manual_id),
            );
            assert!(matches!(
                publish_established_autosave_generation(
                    &root,
                    &leases,
                    capture,
                    ProjectStoreLimits::default(),
                    || false,
                ),
                Err(ProjectStoreFault::Corruption {
                    stage: "autosave_recovery_durability"
                })
            ));
            assert_eq!(root.ref_commit_directory_sync_attempts(), 2);
            assert_eq!(
                fs::read(directory.path().join("refs/autosave-head")).unwrap(),
                old_head
            );
            assert_eq!(
                fs::read(directory.path().join("refs/autosave-recovery")).unwrap(),
                expected_recovery
            );
            assert_eq!(
                fs::read(directory.path().join("refs/head")).unwrap(),
                manual_head
            );
            assert!(leases.confirm_writer(&root).unwrap());

            let (retry, _) = frozen_capture_with_facts(
                "stale.m4dproj",
                &autosave,
                autosave.projection().clone(),
                Some(autosave_id),
                Some(manual_id),
            );
            let receipt = publish_established_autosave_generation(
                &root,
                &leases,
                retry,
                ProjectStoreLimits::default(),
                || false,
            )
            .unwrap();
            assert_eq!(receipt.published_objects(), 0);
            assert_eq!(
                fs::read(directory.path().join("refs/autosave-recovery")).unwrap(),
                expected_recovery
            );
            let head = RefRecord::decode(
                RefKind::AutosaveHead,
                &fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            )
            .unwrap();
            assert_eq!(head.current(), receipt.new_generation_id());
            assert_eq!(head.previous(), Some(autosave_id));
            assert_eq!(head.base(), Some(manual_id));
        }

        for failed_sync in [2, 3] {
            let directory = extracted_frozen_root(
                &format!("established-autosave-head-sync-{failed_sync}"),
                "stale.m4dproj",
            );
            let root = LocalStoreRoot::open(directory.path()).unwrap();
            let leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let old_head = fs::read(directory.path().join("refs/autosave-head")).unwrap();
            let manual_head = fs::read(directory.path().join("refs/head")).unwrap();
            root.fail_ref_commit_directory_sync_at(failed_sync);
            let (capture, _) = frozen_capture_with_facts(
                "stale.m4dproj",
                &autosave,
                autosave.projection().clone(),
                Some(autosave_id),
                Some(manual_id),
            );
            assert!(matches!(
                publish_established_autosave_generation(
                    &root,
                    &leases,
                    capture,
                    ProjectStoreLimits::default(),
                    || fs::read(directory.path().join("refs/autosave-head"))
                        .is_ok_and(|head| head != old_head),
                ),
                Err(ProjectStoreFault::CommitIndeterminate)
            ));
            assert_eq!(root.ref_commit_directory_sync_attempts(), 4);
            assert_eq!(
                fs::read(directory.path().join("refs/autosave-recovery")).unwrap(),
                expected_recovery
            );
            let head = RefRecord::decode(
                RefKind::AutosaveHead,
                &fs::read(directory.path().join("refs/autosave-head")).unwrap(),
            )
            .unwrap();
            assert_ne!(head.current(), autosave_id);
            assert_eq!(head.previous(), Some(autosave_id));
            assert_eq!(head.base(), Some(manual_id));
            assert_eq!(
                fs::read(directory.path().join("refs/head")).unwrap(),
                manual_head
            );
            assert!(matches!(
                leases.confirm_writer(&root),
                Err(LeaseError::Indeterminate)
            ));
        }
    }

    fn capture(
        artifacts: Vec<ArtifactReference>,
        sources: Vec<Box<dyn ProjectObjectSource>>,
    ) -> ProjectCommitCapture {
        let project_id = ProjectId::parse("11111111-2222-4333-8444-555555555555").unwrap();
        let layer = LayerViewState::new(
            LogicalLayerKey::new(0),
            true,
            transfer(),
            RenderState::mip(SamplingPolicy::SmoothLinear),
        );
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            320.0,
            40.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![layer],
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            camera,
            ViewerLayout::Single3d,
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let dataset = DatasetReference::new(
            ScientificContentId::parse(&format!(
                "{}{}",
                ScientificContentId::PREFIX,
                "13".repeat(32)
            ))
            .unwrap(),
            None,
            None,
            None,
        );
        let state = ProjectState::new(project_id, dataset, view, Vec::new(), artifacts).unwrap();
        let projection = ProjectGenerationProjection::new(
            ProjectRevisionId::initial(project_id),
            ProjectRevisionHighWater::initial(project_id),
            state,
        )
        .unwrap();
        ProjectCommitCapture::new(projection, None, None, None, sources).unwrap()
    }

    fn artifact(
        byte: u8,
        schema: ArtifactSchema,
        descriptor: RawObjectDescriptor,
    ) -> ArtifactReference {
        ArtifactReference::new(
            ArtifactHandleId::from_bytes([byte; 16]),
            schema,
            ArtifactContentId::parse(&format!(
                "{}{}",
                ArtifactContentId::PREFIX,
                format!("{byte:02x}").repeat(32)
            ))
            .unwrap(),
            descriptor,
            None,
            None,
            vec![LogicalLayerKey::new(0)],
            "artifact",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        )
        .unwrap()
    }

    fn descriptor(schema: ArtifactSchema, bytes: &[u8]) -> RawObjectDescriptor {
        let facts = ExactBytesHasher::hash(bytes).unwrap();
        RawObjectDescriptor::new(
            facts.digest(),
            facts.byte_length(),
            MediaType::parse(schema.media_type()).unwrap(),
            ObjectRole::parse(schema.object_role()).unwrap(),
        )
    }

    fn transfer() -> LayerTransfer {
        LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        )
    }

    fn generation_path(root: &Path, id: ProjectGenerationId) -> PathBuf {
        let digest = id.digest().to_string();
        root.join("generations")
            .join("sha256")
            .join(&digest[..2])
            .join(format!("{}.json", &digest[2..]))
    }

    fn object_path(root: &Path, digest: ExactBytesDigest) -> PathBuf {
        let digest = digest.digest().to_string();
        root.join("objects")
            .join("sha256")
            .join(&digest[..2])
            .join(&digest[2..])
    }

    fn prepared_frozen_root(label: &str) -> TestDirectory {
        prepared_frozen_root_from_store(label, "stale.m4dproj")
    }

    fn prepared_frozen_root_from_store(label: &str, store: &str) -> TestDirectory {
        let directory = TestDirectory::new(label);
        fs::write(
            directory.path().join("project.json"),
            fixture_extract(&format!("{store}/project.json")),
        )
        .unwrap();
        directory
    }

    fn extracted_frozen_root(label: &str, store: &str) -> TestDirectory {
        let directory = TestDirectory::new(label);
        let output = Command::new("tar")
            .arg("-xzf")
            .arg(fixture_archive())
            .arg("-C")
            .arg(directory.path())
            .arg("--strip-components=1")
            .arg(store)
            .output()
            .unwrap();
        assert!(output.status.success(), "failed to extract {store}");
        directory
    }

    fn frozen_stale_manual() -> (ProjectGenerationId, GenerationDocument, Vec<u8>) {
        let id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let member = fixture_generation_member("stale.m4dproj", id);
        let bytes = fixture_extract(&member);
        let project_id = ProjectId::parse(STALE_PROJECT).unwrap();
        let document =
            GenerationDocument::decode(id, project_id, &bytes, ProjectStoreLimits::default())
                .unwrap();
        (id, document, bytes)
    }

    fn frozen_generation(store: &str, id: &str) -> GenerationDocument {
        let id = ProjectGenerationId::parse(id).unwrap();
        let bytes = fixture_extract(&fixture_generation_member(store, id));
        let envelope =
            ProjectEnvelope::decode(&fixture_extract(&format!("{store}/project.json"))).unwrap();
        GenerationDocument::decode(
            id,
            envelope.project_id(),
            &bytes,
            ProjectStoreLimits::default(),
        )
        .unwrap()
    }

    fn frozen_capture(
        store: &str,
        frozen: &GenerationDocument,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        frozen_capture_with_parent(store, frozen, frozen.parent_generation_id())
    }

    fn frozen_capture_with_parent(
        store: &str,
        frozen: &GenerationDocument,
        expected_parent: Option<ProjectGenerationId>,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        frozen_capture_with_facts(
            store,
            frozen,
            frozen.projection().clone(),
            expected_parent,
            frozen.base_manual_generation_id(),
        )
    }

    fn frozen_capture_with_facts(
        store: &str,
        frozen: &GenerationDocument,
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        frozen_capture_with_all_facts(
            store,
            frozen,
            projection,
            expected_parent,
            autosave_base,
            frozen.forked_from(),
        )
    }

    fn frozen_capture_with_all_facts(
        store: &str,
        frozen: &GenerationDocument,
        projection: ProjectGenerationProjection,
        expected_parent: Option<ProjectGenerationId>,
        autosave_base: Option<ProjectGenerationId>,
        forked_from: Option<(ProjectId, ProjectGenerationId)>,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        let opens = Arc::new(AtomicUsize::new(0));
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        for artifact in frozen.projection().state().artifacts() {
            let storage = frozen.bindings().get(&artifact.object().digest()).unwrap();
            let bytes = match storage {
                ArtifactStorage::Direct { object } => {
                    fixture_extract(&fixture_object_member(store, object.digest()))
                }
                ArtifactStorage::Paged { binding_manifest } => {
                    let binding_bytes =
                        fixture_extract(&fixture_object_member(store, binding_manifest.digest()));
                    let binding = LogicalObjectBinding::decode(
                        &binding_bytes,
                        artifact.object(),
                        binding_manifest,
                        ProjectStoreLimits::default(),
                    )
                    .unwrap();
                    let mut logical = Vec::with_capacity(
                        usize::try_from(artifact.object().byte_length()).unwrap(),
                    );
                    for page in binding.pages() {
                        logical.extend_from_slice(&fixture_extract(&fixture_object_member(
                            store,
                            page.object().digest(),
                        )));
                    }
                    logical
                }
            };
            sources.push(Box::new(MemorySource {
                descriptor: artifact.object().clone(),
                bytes: Arc::<[u8]>::from(bytes),
                opens: Arc::clone(&opens),
                flip_second: false,
            }));
        }
        let capture = ProjectCommitCapture::new(
            projection,
            expected_parent,
            autosave_base,
            forked_from,
            sources,
        )
        .unwrap();
        (capture, opens)
    }

    fn projection_with_revision(
        frozen: &GenerationDocument,
        revision_sequence: u64,
        high_water_sequence: u64,
    ) -> ProjectGenerationProjection {
        let project_id = frozen.projection().state().project_id();
        ProjectGenerationProjection::new(
            ProjectRevisionId::new(project_id, revision_sequence),
            ProjectRevisionHighWater::new(project_id, high_water_sequence),
            frozen.projection().state().clone(),
        )
        .unwrap()
    }

    fn rejected_frozen_capture(frozen: &GenerationDocument) -> ProjectCommitCapture {
        let mut descriptors = BTreeMap::new();
        for artifact in frozen.projection().state().artifacts() {
            descriptors
                .entry(artifact.object().digest())
                .or_insert_with(|| artifact.object().clone());
        }
        let sources = descriptors
            .into_values()
            .map(|descriptor| Box::new(MustNotOpen { descriptor }) as Box<dyn ProjectObjectSource>)
            .collect();
        ProjectCommitCapture::new(
            frozen.projection().clone(),
            frozen.parent_generation_id(),
            frozen.base_manual_generation_id(),
            frozen.forked_from(),
            sources,
        )
        .unwrap()
    }

    fn fixture_extract(member: &str) -> Vec<u8> {
        let output = Command::new("tar")
            .arg("-xOf")
            .arg(fixture_archive())
            .arg(member)
            .output()
            .unwrap();
        assert!(output.status.success(), "failed to extract {member}");
        output.stdout
    }

    fn fixture_archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/project/project-store-v1.tar.gz")
    }

    fn fixture_generation_member(store: &str, id: ProjectGenerationId) -> String {
        let digest = id.digest().to_string();
        format!(
            "{store}/generations/sha256/{}/{}.json",
            &digest[..2],
            &digest[2..]
        )
    }

    fn fixture_object_member(store: &str, digest: ExactBytesDigest) -> String {
        let digest = digest.digest().to_string();
        format!("{store}/objects/sha256/{}/{}", &digest[..2], &digest[2..])
    }
}
