use super::*;
use quick_xml::XmlVersion;

pub(super) fn inspect_tiff_inputs(
    input_dir: &Path,
    source_profile: TiffSourceProfile,
    inputs: Vec<TiffInput>,
) -> Result<TiffDirectoryInspection, ImportError> {
    match source_profile {
        TiffSourceProfile::StackSeriesMovie => {
            inspect_stack_series_inputs(input_dir, source_profile, inputs)
        }
        TiffSourceProfile::PlaneSeriesVolume => {
            inspect_plane_series_inputs(input_dir, source_profile, inputs)
        }
    }
}

pub(super) fn inspect_stack_series_inputs(
    input_dir: &Path,
    source_profile: TiffSourceProfile,
    inputs: Vec<TiffInput>,
) -> Result<TiffDirectoryInspection, ImportError> {
    let file_count = inputs.len();
    let files = inputs
        .iter()
        .map(|input| TiffFileGrouping {
            path: input.path.clone(),
            channel: input.channel,
            stack_index: input.stack_index,
        })
        .collect::<Vec<_>>();
    let grouped = group_by_channel(inputs);
    let first_channel = grouped
        .values()
        .next()
        .expect("discover_tiffs rejects empty input");
    let timepoint_count = first_channel.len();
    let mut expected_shape = None;
    let mut expected_source_dtype = None;
    let mut metadata_accumulator = TiffSourceMetadataAccumulator::default();
    let mut value_range: Option<TiffValueRangeSummary> = None;
    let mut channels = Vec::with_capacity(grouped.len());

    for (channel, channel_inputs) in grouped {
        if channel_inputs.len() != timepoint_count {
            return Err(ImportError::TimepointCountMismatch {
                channel,
                actual: channel_inputs.len(),
                expected: timepoint_count,
            });
        }

        for input in &channel_inputs {
            let stack = read_tiff_stack(&input.path)?;
            let shape = stack.shape;
            if let Some(expected) = expected_shape
                && shape != expected
            {
                return Err(ImportError::StackShapeMismatch {
                    path: input.path.clone(),
                    actual: shape,
                    expected,
                });
            }
            expected_shape = Some(shape);
            if let Some(expected) = expected_source_dtype
                && stack.source_dtype != expected
            {
                return Err(ImportError::SourceDTypeMismatch {
                    path: input.path.clone(),
                    actual: stack.source_dtype,
                    expected,
                });
            }
            expected_source_dtype = Some(stack.source_dtype);
            metadata_accumulator.push(&stack.source_metadata);
            let stack_range = stack.values_zyx.value_range();
            value_range = Some(
                value_range
                    .map(|range| range.merge(stack_range))
                    .unwrap_or(stack_range),
            );
        }

        channels.push(TiffChannelInspection {
            channel,
            timepoint_count: channel_inputs.len() as u64,
        });
    }

    let source_metadata = metadata_accumulator.finish();
    Ok(TiffDirectoryInspection {
        input_dir: input_dir.to_path_buf(),
        source_profile,
        file_count,
        channel_count: channels.len(),
        timepoint_count: timepoint_count as u64,
        shape: expected_shape.expect("discover_tiffs rejects empty input"),
        source_dtype: expected_source_dtype.expect("discover_tiffs rejects empty input"),
        metadata_confidence: tiff_metadata_confidence(&source_metadata),
        source_metadata,
        value_range: value_range.expect("discover_tiffs rejects empty input"),
        files,
        channels,
    })
}

pub(super) fn inspect_plane_series_inputs(
    input_dir: &Path,
    source_profile: TiffSourceProfile,
    inputs: Vec<TiffInput>,
) -> Result<TiffDirectoryInspection, ImportError> {
    let file_count = inputs.len();
    let files = inputs
        .iter()
        .map(|input| TiffFileGrouping {
            path: input.path.clone(),
            channel: input.channel,
            stack_index: input.stack_index,
        })
        .collect::<Vec<_>>();
    let grouped = group_by_channel(inputs);
    let first_channel = grouped
        .values()
        .next()
        .expect("plane-series discovery rejects empty input");
    let expected_plane_count = first_channel.len();
    let mut expected_plane_shape = None;
    let mut expected_source_dtype = None;
    let mut value_range: Option<TiffValueRangeSummary> = None;
    let mut channels = Vec::with_capacity(grouped.len());

    for (channel, channel_inputs) in grouped {
        if channel_inputs.len() != expected_plane_count {
            return Err(ImportError::TimepointCountMismatch {
                channel,
                actual: channel_inputs.len(),
                expected: expected_plane_count,
            });
        }

        for input in &channel_inputs {
            let stack = read_tiff_stack(&input.path)?;
            if stack.shape.z != 1 {
                return Err(ImportError::PlaneSeriesFileHasMultipleImages {
                    path: input.path.clone(),
                    z: stack.shape.z,
                });
            }
            let plane_shape = TiffStackShape {
                z: 1,
                y: stack.shape.y,
                x: stack.shape.x,
            };
            if let Some(expected) = expected_plane_shape
                && plane_shape != expected
            {
                return Err(ImportError::StackShapeMismatch {
                    path: input.path.clone(),
                    actual: plane_shape,
                    expected,
                });
            }
            expected_plane_shape = Some(plane_shape);
            if let Some(expected) = expected_source_dtype
                && stack.source_dtype != expected
            {
                return Err(ImportError::SourceDTypeMismatch {
                    path: input.path.clone(),
                    actual: stack.source_dtype,
                    expected,
                });
            }
            expected_source_dtype = Some(stack.source_dtype);
            let stack_range = stack.values_zyx.value_range();
            value_range = Some(
                value_range
                    .map(|range| range.merge(stack_range))
                    .unwrap_or(stack_range),
            );
        }

        channels.push(TiffChannelInspection {
            channel,
            timepoint_count: 1,
        });
    }

    let plane_shape = expected_plane_shape.expect("plane-series discovery rejects empty input");
    Ok(TiffDirectoryInspection {
        input_dir: input_dir.to_path_buf(),
        source_profile,
        file_count,
        channel_count: channels.len(),
        timepoint_count: 1,
        shape: TiffStackShape {
            z: expected_plane_count as u64,
            y: plane_shape.y,
            x: plane_shape.x,
        },
        source_dtype: expected_source_dtype.expect("plane-series discovery rejects empty input"),
        metadata_confidence: TiffMetadataConfidence::MissingSpatialCalibration,
        source_metadata: TiffSourceMetadata::missing(),
        value_range: value_range.expect("plane-series discovery rejects empty input"),
        files,
        channels,
    })
}

pub(super) fn tiff_metadata_confidence(metadata: &TiffSourceMetadata) -> TiffMetadataConfidence {
    match metadata.voxel_spacing_status {
        TiffVoxelSpacingMetadataStatus::Complete => TiffMetadataConfidence::CompleteOmeXml,
        TiffVoxelSpacingMetadataStatus::Missing => {
            TiffMetadataConfidence::MissingSpatialCalibration
        }
        TiffVoxelSpacingMetadataStatus::Incomplete => {
            TiffMetadataConfidence::IncompleteSpatialCalibration
        }
        TiffVoxelSpacingMetadataStatus::Conflicting => {
            TiffMetadataConfidence::ConflictingSpatialCalibration
        }
    }
}

pub fn accepted_tiff_reviewed_import_plan(
    inspection: &TiffDirectoryInspection,
    voxel_spacing_um: [f64; 3],
    grouping_confirmed: bool,
) -> TiffReviewedImportPlan {
    let mut user_corrections = Vec::new();
    let source_spacing = inspection.source_metadata.voxel_spacing_um;
    if source_spacing
        .is_none_or(|source_spacing| !voxel_spacing_matches(source_spacing, voxel_spacing_um))
    {
        user_corrections.push(TiffUserCorrection {
            field: "voxel_spacing_um".to_owned(),
            source_value: source_spacing.map(|spacing| format!("{spacing:?}")),
            reviewed_value: format!("{voxel_spacing_um:?}"),
            reason: "explicit import review".to_owned(),
        });
    }
    if grouping_confirmed {
        user_corrections.push(TiffUserCorrection {
            field: "source_profile".to_owned(),
            source_value: Some("auto_detected_import_layout".to_owned()),
            reviewed_value: inspection.source_profile.id().to_owned(),
            reason: "explicit import review".to_owned(),
        });
        user_corrections.push(TiffUserCorrection {
            field: "channel_time_grouping".to_owned(),
            source_value: Some("filename_or_review_default".to_owned()),
            reviewed_value: format!(
                "{} file(s), {} channel(s), {} timepoint(s)",
                inspection.file_count, inspection.channel_count, inspection.timepoint_count
            ),
            reason: "explicit import review".to_owned(),
        });
    }
    let (source_format, source_axes) = match inspection.source_profile {
        TiffSourceProfile::StackSeriesMovie => {
            let source_format = match inspection.source_metadata.voxel_spacing_source {
                Some(TiffVoxelSpacingMetadataSource::OmeXml)
                    if inspection.source_metadata.voxel_spacing_status
                        == TiffVoxelSpacingMetadataStatus::Complete =>
                {
                    SOURCE_FORMAT_OME_TIFF
                }
                _ => SOURCE_FORMAT_EXPLICIT_TIFF_STACK,
            };
            (
                source_format,
                vec![
                    "file_time".to_owned(),
                    "z".to_owned(),
                    "y".to_owned(),
                    "x".to_owned(),
                ],
            )
        }
        TiffSourceProfile::PlaneSeriesVolume => (
            SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME,
            vec![
                "channel_folder".to_owned(),
                "plane_file_z".to_owned(),
                "y".to_owned(),
                "x".to_owned(),
            ],
        ),
    };
    TiffReviewedImportPlan {
        review_status: TiffImportReviewStatus::Accepted,
        source_profile: inspection.source_profile,
        source_format: source_format.to_owned(),
        metadata_confidence: inspection.metadata_confidence,
        source_axes,
        native_axes: ["t", "z", "y", "x"].map(str::to_owned).to_vec(),
        channels_as_layers: true,
        value_range: Some(inspection.value_range),
        no_data_policy: None,
        user_corrections,
    }
}

#[derive(Debug, Default)]
pub(super) struct TiffSourceMetadataAccumulator {
    complete_spacing_um: Option<[f64; 3]>,
    complete_source: Option<TiffVoxelSpacingMetadataSource>,
    saw_missing: bool,
    saw_incomplete: bool,
    saw_conflicting: bool,
}

impl TiffSourceMetadataAccumulator {
    fn push(&mut self, metadata: &TiffSourceMetadata) {
        match metadata.voxel_spacing_status {
            TiffVoxelSpacingMetadataStatus::Complete => {
                let Some(spacing_um) = metadata.voxel_spacing_um else {
                    self.saw_incomplete = true;
                    return;
                };
                match self.complete_spacing_um {
                    None => {
                        self.complete_spacing_um = Some(spacing_um);
                        self.complete_source = metadata.voxel_spacing_source;
                    }
                    Some(existing) if voxel_spacing_matches(existing, spacing_um) => {}
                    Some(_) => {
                        self.saw_conflicting = true;
                    }
                }
            }
            TiffVoxelSpacingMetadataStatus::Missing => {
                self.saw_missing = true;
            }
            TiffVoxelSpacingMetadataStatus::Incomplete => {
                self.saw_incomplete = true;
            }
            TiffVoxelSpacingMetadataStatus::Conflicting => {
                self.saw_conflicting = true;
            }
        }
    }

    fn finish(self) -> TiffSourceMetadata {
        if self.saw_conflicting {
            return TiffSourceMetadata {
                voxel_spacing_um: None,
                voxel_spacing_status: TiffVoxelSpacingMetadataStatus::Conflicting,
                voxel_spacing_source: None,
            };
        }

        match self.complete_spacing_um {
            Some(spacing_um) if !self.saw_missing && !self.saw_incomplete => {
                TiffSourceMetadata::complete(
                    spacing_um,
                    self.complete_source
                        .unwrap_or(TiffVoxelSpacingMetadataSource::OmeXml),
                )
            }
            Some(_) if self.saw_missing || self.saw_incomplete => TiffSourceMetadata::incomplete(),
            Some(spacing_um) => TiffSourceMetadata::complete(
                spacing_um,
                self.complete_source
                    .unwrap_or(TiffVoxelSpacingMetadataSource::OmeXml),
            ),
            None if self.saw_incomplete => TiffSourceMetadata::incomplete(),
            None => TiffSourceMetadata::missing(),
        }
    }
}

pub(super) fn voxel_spacing_matches(left: [f64; 3], right: [f64; 3]) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| (*left - right).abs() <= 1.0e-9)
}

pub(super) fn read_tiff_source_metadata<R: std::io::Read + std::io::Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
) -> Result<TiffSourceMetadata, ImportError> {
    match decoder.get_tag_ascii_string(Tag::ImageDescription) {
        Ok(description) => Ok(parse_ome_tiff_source_metadata(&description)),
        Err(tiff::TiffError::FormatError(TiffFormatError::RequiredTagNotFound(
            Tag::ImageDescription,
        ))) => Ok(TiffSourceMetadata::missing()),
        Err(err) => Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: err.to_string(),
        }),
    }
}

pub(super) const MAX_OME_XML_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_OME_XML_EVENTS: usize = 65_536;
pub(super) const MAX_OME_XML_DEPTH: usize = 64;
pub(super) const MAX_OME_XML_ATTRIBUTES_PER_ELEMENT: usize = 256;
pub(super) const MAX_OME_XML_ATTRIBUTE_BYTES_PER_ELEMENT: usize = 64 * 1024;
pub(super) const MAX_OME_XML_TOTAL_ATTRIBUTE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_OME_XML_METADATA_VALUE_BYTES: usize = 256;

pub(super) fn parse_ome_tiff_source_metadata(description: &str) -> TiffSourceMetadata {
    if description.len() > MAX_OME_XML_BYTES {
        return TiffSourceMetadata::incomplete();
    }

    let mut reader = XmlReader::from_str(description);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut event_count = 0usize;
    let mut depth = 0usize;
    let mut total_attribute_bytes = 0usize;

    loop {
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(_) => return TiffSourceMetadata::incomplete(),
        };
        if !matches!(&event, XmlEvent::Eof) {
            event_count = match event_count.checked_add(1) {
                Some(event_count) if event_count <= MAX_OME_XML_EVENTS => event_count,
                _ => return TiffSourceMetadata::incomplete(),
            };
        }

        match event {
            XmlEvent::Start(element) => {
                depth = match depth.checked_add(1) {
                    Some(depth) if depth <= MAX_OME_XML_DEPTH => depth,
                    _ => return TiffSourceMetadata::incomplete(),
                };
                if !account_ome_xml_attributes(&element, &mut total_attribute_bytes) {
                    return TiffSourceMetadata::incomplete();
                }
                if element.local_name().as_ref() == b"Pixels" {
                    return parse_ome_pixels_metadata(&element);
                }
            }
            XmlEvent::Empty(element) => {
                if !account_ome_xml_attributes(&element, &mut total_attribute_bytes) {
                    return TiffSourceMetadata::incomplete();
                }
                if element.local_name().as_ref() == b"Pixels" {
                    return parse_ome_pixels_metadata(&element);
                }
            }
            XmlEvent::End(_) => {
                depth = match depth.checked_sub(1) {
                    Some(depth) => depth,
                    None => return TiffSourceMetadata::incomplete(),
                };
            }
            XmlEvent::Eof if depth == 0 => return TiffSourceMetadata::missing(),
            XmlEvent::Eof => return TiffSourceMetadata::incomplete(),
            _ => {}
        }
        buffer.clear();
    }
}

fn account_ome_xml_attributes(element: &BytesStart<'_>, total_attribute_bytes: &mut usize) -> bool {
    let mut attribute_count = 0usize;
    let mut element_attribute_bytes = 0usize;

    for attribute in element.attributes() {
        let Ok(attribute) = attribute else {
            return false;
        };
        attribute_count = match attribute_count.checked_add(1) {
            Some(attribute_count) if attribute_count <= MAX_OME_XML_ATTRIBUTES_PER_ELEMENT => {
                attribute_count
            }
            _ => return false,
        };
        element_attribute_bytes = match element_attribute_bytes
            .checked_add(attribute.key.as_ref().len())
            .and_then(|bytes| bytes.checked_add(attribute.value.as_ref().len()))
        {
            Some(bytes) if bytes <= MAX_OME_XML_ATTRIBUTE_BYTES_PER_ELEMENT => bytes,
            _ => return false,
        };
    }

    *total_attribute_bytes = match total_attribute_bytes.checked_add(element_attribute_bytes) {
        Some(bytes) if bytes <= MAX_OME_XML_TOTAL_ATTRIBUTE_BYTES => bytes,
        _ => return false,
    };
    true
}

fn parse_ome_pixels_metadata(element: &BytesStart<'_>) -> TiffSourceMetadata {
    let mut size_x = None;
    let mut size_y = None;
    let mut size_z = None;
    let mut unit_x = None;
    let mut unit_y = None;
    let mut unit_z = None;

    // The first pass in `account_ome_xml_attributes` already rejected duplicate
    // names and enforced the count/byte budgets, so this bounded second pass can
    // skip duplicate-name bookkeeping.
    for attribute in element.attributes().with_checks(false) {
        let Ok(attribute) = attribute else {
            return TiffSourceMetadata::incomplete();
        };
        let key = attribute.key.local_name();
        if !matches!(
            key.as_ref(),
            b"PhysicalSizeX"
                | b"PhysicalSizeY"
                | b"PhysicalSizeZ"
                | b"PhysicalSizeXUnit"
                | b"PhysicalSizeYUnit"
                | b"PhysicalSizeZUnit"
        ) {
            continue;
        }
        if attribute.value.as_ref().len() > MAX_OME_XML_METADATA_VALUE_BYTES {
            return TiffSourceMetadata::incomplete();
        }
        let Ok(value) =
            attribute.decoded_and_normalized_value(XmlVersion::Implicit1_0, element.decoder())
        else {
            return TiffSourceMetadata::incomplete();
        };
        match key.as_ref() {
            b"PhysicalSizeX" => size_x = value.parse::<f64>().ok(),
            b"PhysicalSizeY" => size_y = value.parse::<f64>().ok(),
            b"PhysicalSizeZ" => size_z = value.parse::<f64>().ok(),
            b"PhysicalSizeXUnit" => unit_x = Some(value.to_string()),
            b"PhysicalSizeYUnit" => unit_y = Some(value.to_string()),
            b"PhysicalSizeZUnit" => unit_z = Some(value.to_string()),
            _ => {}
        }
    }

    let (Some(size_x), Some(size_y), Some(size_z), Some(unit_x), Some(unit_y), Some(unit_z)) =
        (size_x, size_y, size_z, unit_x, unit_y, unit_z)
    else {
        return TiffSourceMetadata::incomplete();
    };

    let Some(x_um) = physical_size_to_um(size_x, &unit_x) else {
        return TiffSourceMetadata::incomplete();
    };
    let Some(y_um) = physical_size_to_um(size_y, &unit_y) else {
        return TiffSourceMetadata::incomplete();
    };
    let Some(z_um) = physical_size_to_um(size_z, &unit_z) else {
        return TiffSourceMetadata::incomplete();
    };
    if [x_um, y_um, z_um]
        .iter()
        .any(|spacing| !spacing.is_finite() || *spacing <= 0.0)
    {
        return TiffSourceMetadata::incomplete();
    }

    TiffSourceMetadata::complete([x_um, y_um, z_um], TiffVoxelSpacingMetadataSource::OmeXml)
}

pub(super) fn physical_size_to_um(value: f64, unit: &str) -> Option<f64> {
    let normalized = unit.trim().to_ascii_lowercase();
    let scale = match normalized.as_str() {
        "um" | "micrometer" | "micrometre" | "micrometers" | "micrometres" | "micron"
        | "microns" => 1.0,
        "nm" | "nanometer" | "nanometre" | "nanometers" | "nanometres" => 0.001,
        "mm" | "millimeter" | "millimetre" | "millimeters" | "millimetres" => 1000.0,
        "m" | "meter" | "metre" | "meters" | "metres" => 1_000_000.0,
        _ if unit.trim() == "\u{00b5}m" || unit.trim() == "\u{03bc}m" => 1.0,
        _ => return None,
    };
    Some(value * scale)
}

pub(super) fn tiff_source_dtype<R: std::io::Read + std::io::Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
) -> Result<IntensityDType, ImportError> {
    let color_type = decoder.colortype().map_err(|err| ImportError::DecodeTiff {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    let sample_format = decoder
        .image_chunk_buffer_layout(0)
        .map_err(|err| ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?
        .sample_format;

    match (color_type, sample_format) {
        (ColorType::Gray(8), SampleFormat::Uint) => Ok(IntensityDType::Uint8),
        (ColorType::Gray(16), SampleFormat::Uint) => Ok(IntensityDType::Uint16),
        (ColorType::Gray(32), SampleFormat::IEEEFP) => Ok(IntensityDType::Float32),
        _ => Err(ImportError::UnsupportedPixelType {
            path: path.to_path_buf(),
        }),
    }
}
