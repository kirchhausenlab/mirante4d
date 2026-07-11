use super::*;

pub(super) fn prepare_import_output(
    output_package: &Path,
    existing_policy: ExistingPackagePolicy,
) -> Result<PathBuf, ImportError> {
    if output_package.exists() && existing_policy == ExistingPackagePolicy::Fail {
        return Err(ImportError::Format(
            mirante4d_format::FormatError::PackageExists(output_package.to_path_buf()),
        ));
    }

    let temporary_output_package = temporary_output_package_path(output_package);
    if temporary_output_package.exists() {
        remove_existing_path(&temporary_output_package).map_err(|source| {
            ImportError::RemoveTemporaryPackage {
                path: temporary_output_package.clone(),
                source,
            }
        })?;
    }

    let backup_output_package = replacement_backup_package_path(output_package);
    if output_package.exists()
        && existing_policy == ExistingPackagePolicy::Replace
        && backup_output_package.exists()
    {
        remove_existing_path(&backup_output_package).map_err(|source| {
            ImportError::RemoveTemporaryPackage {
                path: backup_output_package.clone(),
                source,
            }
        })?;
    }

    Ok(temporary_output_package)
}

pub(super) fn commit_temporary_package(
    temporary_output_package: &Path,
    output_package: &Path,
    existing_policy: ExistingPackagePolicy,
) -> Result<(), ImportError> {
    if output_package.exists() && existing_policy == ExistingPackagePolicy::Fail {
        return Err(ImportError::Format(
            mirante4d_format::FormatError::PackageExists(output_package.to_path_buf()),
        ));
    }

    let mut backup_guard = if output_package.exists() {
        let backup_path = replacement_backup_package_path(output_package);
        if backup_path.exists() {
            remove_existing_path(&backup_path).map_err(|source| {
                ImportError::RemoveTemporaryPackage {
                    path: backup_path.clone(),
                    source,
                }
            })?;
        }
        fs::rename(output_package, &backup_path).map_err(|source| {
            ImportError::BackupOutputPackage {
                output_path: output_package.to_path_buf(),
                backup_path: backup_path.clone(),
                source,
            }
        })?;
        Some(OutputBackupGuard::new(
            backup_path,
            output_package.to_path_buf(),
        ))
    } else {
        None
    };

    fs::rename(temporary_output_package, output_package).map_err(|source| {
        ImportError::CommitTemporaryPackage {
            temporary_path: temporary_output_package.to_path_buf(),
            output_path: output_package.to_path_buf(),
            source,
        }
    })?;

    if let Some(mut guard) = backup_guard.take() {
        let backup_path = guard.backup_path().to_path_buf();
        guard.disarm();
        if backup_path.exists() {
            remove_existing_path(&backup_path).map_err(|source| {
                ImportError::RemoveOutputPackage {
                    path: backup_path,
                    source,
                }
            })?;
        }
    }

    Ok(())
}

pub(super) fn check_import_cancelled(
    cancellation: &ImportCancellationToken,
) -> Result<(), ImportError> {
    if cancellation.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn validate_reviewed_tiff_import_plan(
    plan: &TiffReviewedImportPlan,
    inspection: &TiffDirectoryInspection,
) -> Result<(), ImportError> {
    if plan.review_status != TiffImportReviewStatus::Accepted {
        return Err(ImportError::UnreviewedImportPlan);
    }
    if !supported_source_format_matrix()
        .iter()
        .any(|entry| entry.id == plan.source_format)
    {
        return Err(ImportError::UnsupportedReviewedSourceFormat(
            plan.source_format.clone(),
        ));
    }
    if plan.source_profile != inspection.source_profile {
        return Err(ImportError::ReviewedSourceProfileMismatch {
            reviewed: plan.source_profile,
            inspected: inspection.source_profile,
        });
    }
    if !source_format_allowed_for_profile(plan.source_format.as_str(), inspection.source_profile) {
        return Err(ImportError::UnsupportedReviewedSourceFormat(
            plan.source_format.clone(),
        ));
    }
    if plan.native_axes != ["t", "z", "y", "x"].map(str::to_owned) {
        return Err(ImportError::InvalidReviewedNativeAxes);
    }
    if !plan.channels_as_layers {
        return Err(ImportError::InvalidReviewedChannelPolicy);
    }
    if let Some(reviewed) = plan.value_range
        && !value_range_matches(reviewed, inspection.value_range)
    {
        return Err(ImportError::ReviewedValueRangeMismatch {
            reviewed,
            inspected: inspection.value_range,
        });
    }
    if let Some(no_data_policy) = plan.no_data_policy
        && (no_data_policy.source_dtype != IntensityDType::Uint8
            || inspection.source_dtype != IntensityDType::Uint8)
    {
        return Err(ImportError::SourceDTypeMismatch {
            path: inspection.input_dir.clone(),
            actual: inspection.source_dtype,
            expected: IntensityDType::Uint8,
        });
    }
    Ok(())
}

pub(super) fn source_format_allowed_for_profile(
    source_format: &str,
    source_profile: TiffSourceProfile,
) -> bool {
    match source_profile {
        TiffSourceProfile::StackSeriesMovie => {
            source_format == SOURCE_FORMAT_OME_TIFF
                || source_format == SOURCE_FORMAT_EXPLICIT_TIFF_STACK
        }
        TiffSourceProfile::PlaneSeriesVolume => {
            source_format == SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME
        }
    }
}

pub(super) fn value_range_matches(
    left: TiffValueRangeSummary,
    right: TiffValueRangeSummary,
) -> bool {
    (left.min - right.min).abs() <= 1.0e-9 && (left.max - right.max).abs() <= 1.0e-9
}

pub(super) fn tiff_import_provenance(
    source: &TiffImportSource,
    output_package: &Path,
    inspection: &TiffDirectoryInspection,
    reviewed_plan: &TiffReviewedImportPlan,
    shape4d: Shape4D,
    voxel_spacing_um: [f64; 3],
) -> Result<NativeDatasetProvenance, ImportError> {
    let source_files = inspection
        .files
        .iter()
        .map(|file| source_file_provenance(&file.path))
        .collect::<Result<Vec<_>, _>>()?;
    let source_metadata = SourceMetadataProvenance {
        source_axes: reviewed_plan.source_axes.clone(),
        native_axes: reviewed_plan.native_axes.clone(),
        channels_as_layers: reviewed_plan.channels_as_layers,
        source_dtype: inspection.source_dtype,
        shape_tzyx: shape4d,
        voxel_spacing_um,
        voxel_spacing_status: format!("{:?}", inspection.source_metadata.voxel_spacing_status),
        voxel_spacing_source: inspection
            .source_metadata
            .voxel_spacing_source
            .map(|source| format!("{source:?}")),
        channel_count: inspection.channel_count,
        timepoint_count: inspection.timepoint_count,
        value_range: ValueRangeProvenance {
            min: inspection.value_range.min,
            max: inspection.value_range.max,
        },
        metadata_confidence: format!("{:?}", reviewed_plan.metadata_confidence),
    };
    let mut user_corrections = reviewed_plan
        .user_corrections
        .iter()
        .map(|correction| UserCorrectionProvenance {
            field: correction.field.clone(),
            source_value: correction.source_value.clone(),
            reviewed_value: correction.reviewed_value.clone(),
            reason: correction.reason.clone(),
        })
        .collect::<Vec<_>>();
    if let Some(policy) = reviewed_plan.no_data_policy {
        user_corrections.push(UserCorrectionProvenance {
            field: "no_data_policy".to_owned(),
            source_value: None,
            reviewed_value: format!(
                "sentinel_value {:?} {}; invisible_with_1_voxel_invalid_dilation",
                policy.source_dtype, policy.source_value_uint8
            ),
            reason: "explicit import review".to_owned(),
        });
    }
    Ok(NativeDatasetProvenance {
        kind: NativeDatasetProvenanceKind::Imported,
        created_at_utc: current_unix_seconds_string(),
        app_name: "mirante4d".to_owned(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        native_schema_version: mirante4d_format::manifest::SCHEMA_VERSION,
        source_format: Some(reviewed_plan.source_format.clone()),
        source_files,
        source_metadata: Some(source_metadata),
        user_corrections,
        storage_policy: StoragePolicyProvenance::default(),
        checksum_policy: Default::default(),
        conversion_policy: format!(
            "lossless source {:?} to native {:?}; output {}",
            inspection.source_dtype,
            inspection.source_dtype,
            output_package.display()
        ),
    })
    .map(|mut provenance| {
        if matches!(source, TiffImportSource::SingleFile(_)) && provenance.source_files.len() == 1 {
            provenance.storage_policy.chunking =
                "import_profile_v1_single_file_axis_aligned_bricks".to_owned();
        }
        provenance
    })
}

pub(super) fn source_file_provenance(path: &Path) -> Result<SourceFileProvenance, ImportError> {
    let metadata = fs::metadata(path).map_err(|source| ImportError::OpenTiff {
        path: path.to_path_buf(),
        source,
    })?;
    let modified_unix_seconds = metadata
        .modified()
        .ok()
        .and_then(system_time_to_unix_seconds);
    Ok(SourceFileProvenance {
        absolute_path: absolute_path_string(path),
        display_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unnamed>")
            .to_owned(),
        file_size_bytes: metadata.len(),
        modified_unix_seconds,
        fingerprint_blake3: Some(blake3_file_hex(path)?),
    })
}

pub(super) fn absolute_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

pub(super) fn blake3_file_hex(path: &Path) -> Result<String, ImportError> {
    let mut file = File::open(path).map_err(|source| ImportError::OpenTiff {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| ImportError::OpenTiff {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn current_unix_seconds_string() -> String {
    current_unix_seconds()
        .map(|seconds| seconds.to_string())
        .unwrap_or_else(|| "not-recorded".to_owned())
}

pub(super) fn current_unix_seconds() -> Option<i64> {
    system_time_to_unix_seconds(SystemTime::now())
}

pub(super) fn system_time_to_unix_seconds(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

pub(super) fn temporary_output_package_path(output_package: &Path) -> PathBuf {
    output_package.with_file_name(format!(
        ".{}.tmp-import",
        output_package
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| "mirante4d-import".into())
    ))
}

pub(super) fn replacement_backup_package_path(output_package: &Path) -> PathBuf {
    output_package.with_file_name(format!(
        ".{}.replace-backup",
        output_package
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| "mirante4d-import".into())
    ))
}

pub(super) fn remove_existing_path(path: &Path) -> Result<(), std::io::Error> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

pub(super) struct TemporaryPackageGuard {
    path: PathBuf,
    active: bool,
}

impl TemporaryPackageGuard {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    pub(super) fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TemporaryPackageGuard {
    fn drop(&mut self) {
        if self.active && self.path.exists() {
            let _ = remove_existing_path(&self.path);
        }
    }
}

pub(super) struct OutputBackupGuard {
    backup_path: PathBuf,
    output_path: PathBuf,
    active: bool,
}

impl OutputBackupGuard {
    fn new(backup_path: PathBuf, output_path: PathBuf) -> Self {
        Self {
            backup_path,
            output_path,
            active: true,
        }
    }

    fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for OutputBackupGuard {
    fn drop(&mut self) {
        if self.active && self.backup_path.exists() && !self.output_path.exists() {
            let _ = fs::rename(&self.backup_path, &self.output_path);
        }
    }
}
