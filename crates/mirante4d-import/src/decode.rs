use super::*;

pub(super) fn read_checked_tiff_stack(
    path: &Path,
    expected_shape: TiffStackShape,
    expected_source_dtype: IntensityDType,
) -> Result<TiffStack, ImportError> {
    let stack = read_tiff_stack(path)?;
    if stack.shape != expected_shape {
        return Err(ImportError::StackShapeMismatch {
            path: path.to_path_buf(),
            actual: stack.shape,
            expected: expected_shape,
        });
    }
    if stack.source_dtype != expected_source_dtype {
        return Err(ImportError::SourceDTypeMismatch {
            path: path.to_path_buf(),
            actual: stack.source_dtype,
            expected: expected_source_dtype,
        });
    }
    Ok(stack)
}

pub(super) fn read_tiff_stack(path: &Path) -> Result<TiffStack, ImportError> {
    let file = File::open(path).map_err(|source| ImportError::OpenTiff {
        path: path.to_path_buf(),
        source,
    })?;
    let mut decoder =
        Decoder::new(BufReader::new(file)).map_err(|err| ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
    let source_metadata = read_tiff_source_metadata(path, &mut decoder)?;
    let mut values_u8 = Vec::new();
    let mut values_u16 = Vec::new();
    let mut values_f32 = Vec::new();
    let mut expected_xy = None;
    let mut expected_source_dtype = None;
    let mut z = 0u64;

    loop {
        let dimensions = decoder
            .dimensions()
            .map_err(|err| ImportError::DecodeTiff {
                path: path.to_path_buf(),
                message: err.to_string(),
            })?;
        if let Some(expected) = expected_xy
            && dimensions != expected
        {
            return Err(ImportError::StackShapeMismatch {
                path: path.to_path_buf(),
                actual: TiffStackShape {
                    z: z + 1,
                    y: u64::from(dimensions.1),
                    x: u64::from(dimensions.0),
                },
                expected: TiffStackShape {
                    z: z + 1,
                    y: u64::from(expected.1),
                    x: u64::from(expected.0),
                },
            });
        }
        expected_xy = Some(dimensions);

        let source_dtype = tiff_source_dtype(path, &mut decoder)?;
        if let Some(expected) = expected_source_dtype
            && source_dtype != expected
        {
            return Err(ImportError::SourceDTypeMismatch {
                path: path.to_path_buf(),
                actual: source_dtype,
                expected,
            });
        }
        expected_source_dtype = Some(source_dtype);

        let image_voxels = checked_image_voxel_count(path, dimensions)?;
        match source_dtype {
            IntensityDType::Uint8 => {
                let stack_offset = values_u8.len();
                values_u8.resize(stack_offset + image_voxels, 0);
                read_tiff_image_chunks_into_u8_stack(
                    path,
                    &mut decoder,
                    dimensions,
                    &mut values_u8[stack_offset..],
                )?;
            }
            IntensityDType::Uint16 => {
                let stack_offset = values_u16.len();
                values_u16.resize(stack_offset + image_voxels, 0);
                read_tiff_image_chunks_into_u16_stack(
                    path,
                    &mut decoder,
                    dimensions,
                    &mut values_u16[stack_offset..],
                )?;
            }
            IntensityDType::Float32 => {
                let stack_offset = values_f32.len();
                values_f32.resize(stack_offset + image_voxels, 0.0);
                read_tiff_image_chunks_into_f32_stack(
                    path,
                    &mut decoder,
                    dimensions,
                    &mut values_f32[stack_offset..],
                )?;
            }
        }
        z += 1;
        if decoder.more_images() {
            decoder
                .next_image()
                .map_err(|err| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
        } else {
            break;
        }
    }

    let (x, y) = expected_xy.expect("decoder always reads at least one image");
    let source_dtype = expected_source_dtype.expect("decoder always reads at least one image");
    let values_zyx = match source_dtype {
        IntensityDType::Uint8 => TiffStackValues::U8(values_u8),
        IntensityDType::Uint16 => TiffStackValues::U16(values_u16),
        IntensityDType::Float32 => TiffStackValues::F32(values_f32),
    };
    Ok(TiffStack {
        shape: TiffStackShape {
            z,
            y: u64::from(y),
            x: u64::from(x),
        },
        source_dtype,
        source_metadata,
        values_zyx,
    })
}

pub(super) fn checked_image_voxel_count(
    path: &Path,
    dimensions: (u32, u32),
) -> Result<usize, ImportError> {
    let voxels = u64::from(dimensions.0)
        .checked_mul(u64::from(dimensions.1))
        .ok_or_else(|| ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: "TIFF image dimensions overflow".to_owned(),
        })?;
    usize::try_from(voxels).map_err(|err| ImportError::DecodeTiff {
        path: path.to_path_buf(),
        message: err.to_string(),
    })
}

pub(super) fn read_tiff_image_chunks_into_u8_stack<R: std::io::Read + std::io::Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    dimensions: (u32, u32),
    destination: &mut [u8],
) -> Result<(), ImportError> {
    let image_width = dimensions.0;
    let image_height = dimensions.1;
    let expected = checked_image_voxel_count(path, dimensions)?;
    if destination.len() != expected {
        return Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: format!(
                "internal TIFF stack buffer has {} voxels, expected {expected}",
                destination.len()
            ),
        });
    }

    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    if chunk_width == 0 || chunk_height == 0 {
        return Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: "TIFF chunk dimensions must be nonzero".to_owned(),
        });
    }
    let chunks_across = image_width.div_ceil(chunk_width);
    let chunks_down = image_height.div_ceil(chunk_height);

    for chunk_y in 0..chunks_down {
        for chunk_x in 0..chunks_across {
            let chunk_index = chunk_y
                .checked_mul(chunks_across)
                .and_then(|base| base.checked_add(chunk_x))
                .ok_or_else(|| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: "TIFF chunk index overflow".to_owned(),
                })?;
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
            let chunk = decoder
                .read_chunk(chunk_index)
                .map_err(|err| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
            copy_tiff_chunk_into_u8_stack(
                path,
                chunk,
                data_width,
                data_height,
                chunk_x * chunk_width,
                chunk_y * chunk_height,
                image_width,
                destination,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_tiff_image_chunks_into_u16_stack<R: std::io::Read + std::io::Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    dimensions: (u32, u32),
    destination: &mut [u16],
) -> Result<(), ImportError> {
    let image_width = dimensions.0;
    let image_height = dimensions.1;
    let expected = checked_image_voxel_count(path, dimensions)?;
    if destination.len() != expected {
        return Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: format!(
                "internal TIFF stack buffer has {} voxels, expected {expected}",
                destination.len()
            ),
        });
    }

    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    if chunk_width == 0 || chunk_height == 0 {
        return Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: "TIFF chunk dimensions must be nonzero".to_owned(),
        });
    }
    let chunks_across = image_width.div_ceil(chunk_width);
    let chunks_down = image_height.div_ceil(chunk_height);

    for chunk_y in 0..chunks_down {
        for chunk_x in 0..chunks_across {
            let chunk_index = chunk_y
                .checked_mul(chunks_across)
                .and_then(|base| base.checked_add(chunk_x))
                .ok_or_else(|| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: "TIFF chunk index overflow".to_owned(),
                })?;
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
            let chunk = decoder
                .read_chunk(chunk_index)
                .map_err(|err| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
            copy_tiff_chunk_into_u16_stack(
                path,
                chunk,
                data_width,
                data_height,
                chunk_x * chunk_width,
                chunk_y * chunk_height,
                image_width,
                destination,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_tiff_image_chunks_into_f32_stack<R: std::io::Read + std::io::Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    dimensions: (u32, u32),
    destination: &mut [f32],
) -> Result<(), ImportError> {
    let image_width = dimensions.0;
    let image_height = dimensions.1;
    let expected = checked_image_voxel_count(path, dimensions)?;
    if destination.len() != expected {
        return Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: format!(
                "internal TIFF stack buffer has {} voxels, expected {expected}",
                destination.len()
            ),
        });
    }

    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    if chunk_width == 0 || chunk_height == 0 {
        return Err(ImportError::DecodeTiff {
            path: path.to_path_buf(),
            message: "TIFF chunk dimensions must be nonzero".to_owned(),
        });
    }
    let chunks_across = image_width.div_ceil(chunk_width);
    let chunks_down = image_height.div_ceil(chunk_height);

    for chunk_y in 0..chunks_down {
        for chunk_x in 0..chunks_across {
            let chunk_index = chunk_y
                .checked_mul(chunks_across)
                .and_then(|base| base.checked_add(chunk_x))
                .ok_or_else(|| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: "TIFF chunk index overflow".to_owned(),
                })?;
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
            let chunk = decoder
                .read_chunk(chunk_index)
                .map_err(|err| ImportError::DecodeTiff {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
            copy_tiff_chunk_into_f32_stack(
                path,
                chunk,
                data_width,
                data_height,
                chunk_x * chunk_width,
                chunk_y * chunk_height,
                image_width,
                destination,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn copy_tiff_chunk_into_u8_stack(
    path: &Path,
    chunk: DecodingResult,
    data_width: u32,
    data_height: u32,
    x_start: u32,
    y_start: u32,
    image_width: u32,
    destination: &mut [u8],
) -> Result<(), ImportError> {
    let expected = checked_image_voxel_count(path, (data_width, data_height))?;
    match chunk {
        DecodingResult::U8(values) => {
            if values.len() != expected {
                return Err(tiff_chunk_size_error(path, values.len(), expected));
            }
            for row in 0..data_height as usize {
                let src_start = row * data_width as usize;
                let dst_start =
                    ((y_start as usize + row) * image_width as usize) + x_start as usize;
                destination[dst_start..dst_start + data_width as usize]
                    .copy_from_slice(&values[src_start..src_start + data_width as usize]);
            }
            Ok(())
        }
        _ => Err(ImportError::UnsupportedPixelType {
            path: path.to_path_buf(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn copy_tiff_chunk_into_u16_stack(
    path: &Path,
    chunk: DecodingResult,
    data_width: u32,
    data_height: u32,
    x_start: u32,
    y_start: u32,
    image_width: u32,
    destination: &mut [u16],
) -> Result<(), ImportError> {
    let expected = checked_image_voxel_count(path, (data_width, data_height))?;
    match chunk {
        DecodingResult::U16(values) => {
            if values.len() != expected {
                return Err(tiff_chunk_size_error(path, values.len(), expected));
            }
            for row in 0..data_height as usize {
                let src_start = row * data_width as usize;
                let dst_start =
                    ((y_start as usize + row) * image_width as usize) + x_start as usize;
                destination[dst_start..dst_start + data_width as usize]
                    .copy_from_slice(&values[src_start..src_start + data_width as usize]);
            }
            Ok(())
        }
        _ => Err(ImportError::UnsupportedPixelType {
            path: path.to_path_buf(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn copy_tiff_chunk_into_f32_stack(
    path: &Path,
    chunk: DecodingResult,
    data_width: u32,
    data_height: u32,
    x_start: u32,
    y_start: u32,
    image_width: u32,
    destination: &mut [f32],
) -> Result<(), ImportError> {
    let expected = checked_image_voxel_count(path, (data_width, data_height))?;
    match chunk {
        DecodingResult::F32(values) => {
            if values.len() != expected {
                return Err(tiff_chunk_size_error(path, values.len(), expected));
            }
            for row in 0..data_height as usize {
                let src_start = row * data_width as usize;
                let dst_start =
                    ((y_start as usize + row) * image_width as usize) + x_start as usize;
                destination[dst_start..dst_start + data_width as usize]
                    .copy_from_slice(&values[src_start..src_start + data_width as usize]);
            }
            Ok(())
        }
        _ => Err(ImportError::UnsupportedPixelType {
            path: path.to_path_buf(),
        }),
    }
}

pub(super) fn tiff_chunk_size_error(path: &Path, actual: usize, expected: usize) -> ImportError {
    ImportError::DecodeTiff {
        path: path.to_path_buf(),
        message: format!("decoded TIFF chunk has {actual} samples, expected {expected}"),
    }
}
