use super::*;

pub fn estimate_tiff_import_storage(
    inspection: &TiffDirectoryInspection,
) -> Result<TiffImportStorageEstimate, ImportError> {
    let shape4d = Shape4D::new(
        inspection.timepoint_count,
        inspection.shape.z,
        inspection.shape.y,
        inspection.shape.x,
    )?;
    let scale_specs = build_mean_multiscale_specs(shape4d, GridToWorld::scale_um(1.0, 1.0, 1.0))?;
    let bytes_per_voxel = import_stored_bytes_per_voxel(inspection.source_dtype);
    let channel_count = u64::try_from(inspection.channel_count)
        .map_err(|_| ImportError::StorageEstimateOverflow)?;

    let mut source_payload_bytes = 0u64;
    let mut derived_multiscale_payload_bytes = 0u64;
    let mut largest_scale_timepoint_bytes = 0u64;
    for scale in &scale_specs {
        let scale_voxels = scale.shape.element_count()?;
        let scale_payload = checked_import_bytes(
            checked_import_bytes(scale_voxels, bytes_per_voxel)?,
            channel_count,
        )?;
        if scale.level == 0 {
            source_payload_bytes = scale_payload;
        } else {
            derived_multiscale_payload_bytes =
                checked_import_sum(derived_multiscale_payload_bytes, scale_payload)?;
        }

        let scale_timepoint_voxels = spatial_voxels_per_timepoint(scale.shape);
        let scale_timepoint_bytes = checked_import_bytes(scale_timepoint_voxels, bytes_per_voxel)?;
        largest_scale_timepoint_bytes = largest_scale_timepoint_bytes.max(scale_timepoint_bytes);
    }

    let scale_count =
        u64::try_from(scale_specs.len()).map_err(|_| ImportError::StorageEstimateOverflow)?;
    let scale_metadata_bytes = checked_import_bytes(
        checked_import_bytes(channel_count, scale_count)?,
        IMPORT_ESTIMATED_SCALE_METADATA_BYTES,
    )?;
    let estimated_metadata_bytes =
        checked_import_sum(IMPORT_ESTIMATED_FIXED_METADATA_BYTES, scale_metadata_bytes)?;
    let estimated_total_bytes = checked_import_sum(
        checked_import_sum(source_payload_bytes, derived_multiscale_payload_bytes)?,
        estimated_metadata_bytes,
    )?;

    Ok(TiffImportStorageEstimate {
        source_payload_bytes,
        derived_multiscale_payload_bytes,
        estimated_metadata_bytes,
        estimated_total_bytes,
        peak_working_stack_bytes: largest_scale_timepoint_bytes,
    })
}

pub fn import_tiff_directory(
    options: TiffDirectoryImportOptions,
) -> Result<TiffDirectoryImportReport, ImportError> {
    let cancellation = ImportCancellationToken::new();
    import_tiff_directory_with_progress(options, &cancellation, |_| Ok(()))
}

pub fn import_tiff_source(
    options: TiffSourceImportOptions,
) -> Result<TiffDirectoryImportReport, ImportError> {
    let cancellation = ImportCancellationToken::new();
    import_tiff_source_with_progress(options, &cancellation, |_| Ok(()))
}

pub fn import_tiff_directory_with_progress<F>(
    options: TiffDirectoryImportOptions,
    cancellation: &ImportCancellationToken,
    progress: F,
) -> Result<TiffDirectoryImportReport, ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let TiffDirectoryImportOptions {
        input_dir,
        output_package,
        dataset_id,
        dataset_name,
        voxel_spacing_um,
        channel_metadata,
        file_grouping,
        existing_policy,
        storage,
        reviewed_plan,
    } = options;
    import_tiff_source_with_progress(
        TiffSourceImportOptions {
            source: TiffImportSource::Directory(input_dir),
            output_package,
            dataset_id,
            dataset_name,
            voxel_spacing_um,
            channel_metadata,
            file_grouping,
            existing_policy,
            storage,
            reviewed_plan,
        },
        cancellation,
        progress,
    )
}

pub fn import_tiff_source_with_progress<F>(
    options: TiffSourceImportOptions,
    cancellation: &ImportCancellationToken,
    mut progress: F,
) -> Result<TiffDirectoryImportReport, ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let TiffSourceImportOptions {
        source,
        output_package,
        dataset_id,
        dataset_name,
        voxel_spacing_um,
        channel_metadata,
        file_grouping,
        existing_policy,
        storage,
        reviewed_plan,
    } = options;
    let temporary_output_package = prepare_import_output(&output_package, existing_policy)?;
    check_import_cancelled(cancellation)?;

    let inputs = discover_tiff_source_for_import(
        &source,
        file_grouping.as_deref(),
        reviewed_plan.source_profile,
    )?;
    progress(ImportProgressEvent::DiscoveredInput {
        file_count: inputs.len(),
    })?;
    check_import_cancelled(cancellation)?;

    let input_count = inputs.len();
    let inspection =
        inspect_tiff_inputs(source.path(), reviewed_plan.source_profile, inputs.clone())?;
    validate_reviewed_tiff_import_plan(&reviewed_plan, &inspection)?;
    let grouped = group_by_channel(inputs);
    let timepoint_count = inspection.timepoint_count;
    let channel_count = inspection.channel_count;
    let expected_shape = inspection.shape;
    let shape4d = Shape4D::new(
        timepoint_count,
        expected_shape.z,
        expected_shape.y,
        expected_shape.x,
    )?;
    let grid_to_world = GridToWorld::scale_um(
        voxel_spacing_um[0],
        voxel_spacing_um[1],
        voxel_spacing_um[2],
    );
    let scale_specs = build_mean_multiscale_specs_with_storage(shape4d, grid_to_world, storage)?;
    let scale_count = scale_specs.len() as u32;
    let storage_estimate = estimate_tiff_import_storage(&inspection)?;
    progress(ImportProgressEvent::EstimatedStorage {
        estimate: storage_estimate,
    })?;
    check_import_cancelled(cancellation)?;
    let mut completed_inputs = 0usize;
    let mut temporary_guard = TemporaryPackageGuard::new(temporary_output_package.clone());
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &temporary_output_package,
        dataset_id,
        dataset_name,
        WorldSpace {
            name: "sample".to_owned(),
            unit: WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )?;
    writer.set_provenance(tiff_import_provenance(
        &source,
        &output_package,
        &inspection,
        &reviewed_plan,
        shape4d,
        voxel_spacing_um,
    )?);

    for (channel, channel_inputs) in grouped {
        let channel_override = channel_metadata.get(&channel);
        let default_channel_metadata = default_tiff_channel_metadata_override(channel);
        let default_channel_name = default_channel_metadata.name;
        let default_channel_color = default_channel_metadata.color_rgba;
        let layer_id = format!("ch{channel}");
        let layer_name = channel_override
            .map(|metadata| metadata.name.clone())
            .unwrap_or(default_channel_name);
        let layer_channel = ChannelMetadata {
            index: channel,
            color_rgba: channel_override
                .map(|metadata| metadata.color_rgba)
                .unwrap_or(default_channel_color),
        };

        match inspection.source_dtype {
            IntensityDType::Uint16 => {
                let mut layer_writer = writer.begin_streaming_layer(StreamingU16LayerSpec {
                    id: layer_id,
                    name: layer_name,
                    channel: layer_channel,
                    source_dtype: inspection.source_dtype,
                    shape: shape4d,
                    grid_to_world,
                    display: default_u16_display(),
                    scales: scale_specs.clone(),
                })?;

                if inspection.source_profile == TiffSourceProfile::PlaneSeriesVolume {
                    write_u16_plane_series_multiscales(
                        &mut layer_writer,
                        &scale_specs,
                        channel_inputs,
                        PlaneSeriesWriteContext {
                            channel,
                            expected_shape,
                            completed_inputs: &mut completed_inputs,
                            input_count,
                            cancellation,
                        },
                        &mut progress,
                    )?;
                } else {
                    for (timepoint_index, input) in channel_inputs.into_iter().enumerate() {
                        check_import_cancelled(cancellation)?;
                        let stack = read_checked_tiff_stack(
                            &input.path,
                            expected_shape,
                            inspection.source_dtype,
                        )?;
                        completed_inputs += 1;
                        progress(ImportProgressEvent::ReadStack {
                            completed: completed_inputs,
                            total: input_count,
                            path: input.path.clone(),
                        })?;
                        check_import_cancelled(cancellation)?;
                        let values_zyx = match stack.values_zyx {
                            TiffStackValues::U16(values) => values,
                            other => {
                                return Err(ImportError::SourceDTypeMismatch {
                                    path: input.path.clone(),
                                    actual: other.dtype(),
                                    expected: IntensityDType::Uint16,
                                });
                            }
                        };
                        write_stack_multiscales(
                            channel,
                            timepoint_index as u64,
                            &mut layer_writer,
                            &scale_specs,
                            values_zyx,
                            cancellation,
                            &mut progress,
                        )?;
                    }
                }

                let source_statistics = layer_writer.scale_statistics(0)?;
                layer_writer.set_display(display_from_statistics(&source_statistics)?);
                writer.finish_streaming_layer(layer_writer)?;
            }
            IntensityDType::Uint8 => {
                let scale_specs_u8 = u8_scale_specs_from_u16(&scale_specs);
                let reviewed_no_data_policy = reviewed_plan.no_data_policy;
                let native_no_data_policy = reviewed_no_data_policy.map(|policy| NoDataPolicy {
                    kind: NoDataPolicyKind::SentinelValue,
                    source_value: f64::from(policy.source_value_uint8),
                    source_dtype: policy.source_dtype,
                    visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
                });
                let mut layer_writer = writer.begin_streaming_u8_layer(StreamingU8LayerSpec {
                    id: layer_id,
                    name: layer_name,
                    channel: layer_channel,
                    shape: shape4d,
                    no_data_policy: native_no_data_policy,
                    grid_to_world,
                    display: default_u8_display(),
                    scales: scale_specs_u8.clone(),
                })?;

                if inspection.source_profile == TiffSourceProfile::PlaneSeriesVolume {
                    write_u8_plane_series_multiscales(
                        &mut layer_writer,
                        &scale_specs_u8,
                        channel_inputs,
                        reviewed_no_data_policy,
                        PlaneSeriesWriteContext {
                            channel,
                            expected_shape,
                            completed_inputs: &mut completed_inputs,
                            input_count,
                            cancellation,
                        },
                        &mut progress,
                    )?;
                } else {
                    for (timepoint_index, input) in channel_inputs.into_iter().enumerate() {
                        check_import_cancelled(cancellation)?;
                        let stack = read_checked_tiff_stack(
                            &input.path,
                            expected_shape,
                            inspection.source_dtype,
                        )?;
                        completed_inputs += 1;
                        progress(ImportProgressEvent::ReadStack {
                            completed: completed_inputs,
                            total: input_count,
                            path: input.path.clone(),
                        })?;
                        check_import_cancelled(cancellation)?;
                        let values_zyx = match stack.values_zyx {
                            TiffStackValues::U8(values) => values,
                            other => {
                                return Err(ImportError::SourceDTypeMismatch {
                                    path: input.path.clone(),
                                    actual: other.dtype(),
                                    expected: IntensityDType::Uint8,
                                });
                            }
                        };
                        write_u8_stack_multiscales(
                            U8StackMultiscaleWrite {
                                channel,
                                timepoint: timepoint_index as u64,
                                scale_specs: &scale_specs_u8,
                                no_data_policy: reviewed_no_data_policy,
                                cancellation,
                            },
                            &mut layer_writer,
                            values_zyx,
                            &mut progress,
                        )?;
                    }
                }

                let source_statistics = layer_writer.scale_statistics(0)?;
                layer_writer.set_display(display_from_statistics(&source_statistics)?);
                writer.finish_streaming_u8_layer(layer_writer)?;
            }
            IntensityDType::Float32 => {
                let scale_specs_f32 = f32_scale_specs_from_u16(&scale_specs);
                let mut layer_writer = writer.begin_streaming_f32_layer(StreamingF32LayerSpec {
                    id: layer_id,
                    name: layer_name,
                    channel: layer_channel,
                    shape: shape4d,
                    grid_to_world,
                    display: default_f32_display(),
                    scales: scale_specs_f32.clone(),
                })?;

                if inspection.source_profile == TiffSourceProfile::PlaneSeriesVolume {
                    write_f32_plane_series_multiscales(
                        &mut layer_writer,
                        &scale_specs_f32,
                        channel_inputs,
                        PlaneSeriesWriteContext {
                            channel,
                            expected_shape,
                            completed_inputs: &mut completed_inputs,
                            input_count,
                            cancellation,
                        },
                        &mut progress,
                    )?;
                } else {
                    for (timepoint_index, input) in channel_inputs.into_iter().enumerate() {
                        check_import_cancelled(cancellation)?;
                        let stack = read_checked_tiff_stack(
                            &input.path,
                            expected_shape,
                            inspection.source_dtype,
                        )?;
                        completed_inputs += 1;
                        progress(ImportProgressEvent::ReadStack {
                            completed: completed_inputs,
                            total: input_count,
                            path: input.path.clone(),
                        })?;
                        check_import_cancelled(cancellation)?;
                        let values_zyx = match stack.values_zyx {
                            TiffStackValues::F32(values) => values,
                            other => {
                                return Err(ImportError::SourceDTypeMismatch {
                                    path: input.path.clone(),
                                    actual: other.dtype(),
                                    expected: IntensityDType::Float32,
                                });
                            }
                        };
                        write_f32_stack_multiscales(
                            channel,
                            timepoint_index as u64,
                            &mut layer_writer,
                            &scale_specs_f32,
                            values_zyx,
                            cancellation,
                            &mut progress,
                        )?;
                    }
                }

                let source_statistics = layer_writer.scale_statistics(0)?;
                layer_writer.set_display(display_from_statistics(&source_statistics)?);
                writer.finish_streaming_f32_layer(layer_writer)?;
            }
        }
    }

    progress(ImportProgressEvent::WritingPackage {
        output_package: output_package.clone(),
    })?;
    check_import_cancelled(cancellation)?;
    writer.finish()?;
    load_and_validate_dataset(&temporary_output_package)?;
    commit_temporary_package(&temporary_output_package, &output_package, existing_policy)?;
    temporary_guard.disarm();
    progress(ImportProgressEvent::Finished {
        output_package: output_package.clone(),
    })?;

    Ok(TiffDirectoryImportReport {
        output_package,
        channel_count,
        timepoint_count,
        scale_count,
        z_planes: expected_shape.z,
        width: expected_shape.x,
        height: expected_shape.y,
    })
}
