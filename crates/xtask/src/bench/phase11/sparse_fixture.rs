use super::*;

pub(super) fn write_phase11_sparse_empty_package(output_root: &Path) -> anyhow::Result<PathBuf> {
    let package = output_root.join("phase11-large-sparse-empty-bricks.m4d");
    let s0_shape = Shape4D::new(1, 128, 128, 128)?;
    let chunk_shape = Shape4D::new(1, 16, 16, 16)?;
    let s0_grid_to_world = GridToWorld::scale_um(0.2, 0.2, 0.2);
    let s0_values = phase11_sparse_empty_values(s0_shape)?;
    let (s1_shape, s1_values) = phase11_downsample_u16_mean2(s0_shape, &s0_values)?;
    let (s2_shape, s2_values) = phase11_downsample_u16_mean2(s1_shape, &s1_values)?;
    let s1_grid_to_world = s0_grid_to_world.downsampled_integer_centered(2, 2, 2)?;
    let s2_grid_to_world = s1_grid_to_world.downsampled_integer_centered(2, 2, 2)?;

    write_native_u16_multiscale_dataset(
        &package,
        NativeU16MultiscaleDataset {
            id: "phase11-large-sparse-empty-bricks".to_owned(),
            name: "Phase 11 large sparse empty-brick synthetic dataset".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: chunk_shape,
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: s0_values,
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: chunk_shape,
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: s1_values,
                    },
                    DenseU16Scale {
                        level: 2,
                        shape: s2_shape,
                        brick_shape: chunk_shape,
                        grid_to_world: s2_grid_to_world,
                        source_scale: Some(1),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: s2_values,
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )?;
    Ok(package)
}

pub(super) fn phase11_sparse_empty_fixture_metadata() -> anyhow::Result<Value> {
    let s0_shape = Shape4D::new(1, 128, 128, 128)?;
    let chunk_shape = Shape4D::new(1, 16, 16, 16)?;
    let s0_values = phase11_sparse_empty_values(s0_shape)?;
    let (s1_shape, s1_values) = phase11_downsample_u16_mean2(s0_shape, &s0_values)?;
    let (s2_shape, s2_values) = phase11_downsample_u16_mean2(s1_shape, &s1_values)?;
    Ok(json!({
        "source_shape": {
            "t": s0_shape.t,
            "z": s0_shape.z,
            "y": s0_shape.y,
            "x": s0_shape.x,
        },
        "chunk_shape": {
            "t": chunk_shape.t,
            "z": chunk_shape.z,
            "y": chunk_shape.y,
            "x": chunk_shape.x,
        },
        "scales": [
            phase11_sparse_scale_metadata(0, s0_shape, &s0_values, chunk_shape)?,
            phase11_sparse_scale_metadata(1, s1_shape, &s1_values, chunk_shape)?,
            phase11_sparse_scale_metadata(2, s2_shape, &s2_values, chunk_shape)?,
        ],
    }))
}

fn phase11_sparse_scale_metadata(
    level: u32,
    shape: Shape4D,
    values: &[u16],
    brick_shape: Shape4D,
) -> anyhow::Result<Value> {
    let (occupied, total) = phase11_sparse_occupied_brick_count(values, shape, brick_shape)?;
    Ok(json!({
        "level": level,
        "shape": {
            "t": shape.t,
            "z": shape.z,
            "y": shape.y,
            "x": shape.x,
        },
        "brick_count": total,
        "occupied_bricks": occupied,
        "empty_bricks": total.saturating_sub(occupied),
    }))
}

pub(super) fn phase11_sparse_empty_values(shape: Shape4D) -> anyhow::Result<Vec<u16>> {
    if shape.t != 1 || shape.z < 112 || shape.y < 112 || shape.x < 112 {
        bail!("Phase 11 sparse fixture requires t=1 and at least 112 voxels on every spatial axis");
    }
    let element_count = usize::try_from(shape.element_count()?)
        .context("Phase 11 sparse fixture shape does not fit usize")?;
    let mut values = vec![0; element_count];
    phase11_fill_sparse_box(&mut values, shape, (24, 40), (48, 64), (48, 64), 20_000)?;
    phase11_fill_sparse_box(&mut values, shape, (96, 104), (96, 104), (16, 24), 42_000)?;
    Ok(values)
}

fn phase11_fill_sparse_box(
    values: &mut [u16],
    shape: Shape4D,
    z_range: (u64, u64),
    y_range: (u64, u64),
    x_range: (u64, u64),
    base_value: u16,
) -> anyhow::Result<()> {
    if z_range.0 >= z_range.1
        || y_range.0 >= y_range.1
        || x_range.0 >= x_range.1
        || z_range.1 > shape.z
        || y_range.1 > shape.y
        || x_range.1 > shape.x
    {
        bail!("invalid Phase 11 sparse fixture box for shape {shape:?}");
    }
    for z in z_range.0..z_range.1 {
        for y in y_range.0..y_range.1 {
            for x in x_range.0..x_range.1 {
                let local_pattern = ((z * 31 + y * 17 + x * 7) % 2048) as u16;
                let index = phase11_tzyx_index(shape, 0, z, y, x)?;
                values[index] = base_value.saturating_add(local_pattern);
            }
        }
    }
    Ok(())
}

pub(super) fn phase11_downsample_u16_mean2(
    source_shape: Shape4D,
    source_values: &[u16],
) -> anyhow::Result<(Shape4D, Vec<u16>)> {
    let expected_source_len = usize::try_from(source_shape.element_count()?)
        .context("Phase 11 downsample source shape does not fit usize")?;
    if source_values.len() != expected_source_len {
        bail!(
            "Phase 11 downsample source length mismatch: got {}, expected {}",
            source_values.len(),
            expected_source_len
        );
    }
    if !source_shape.z.is_multiple_of(2)
        || !source_shape.y.is_multiple_of(2)
        || !source_shape.x.is_multiple_of(2)
    {
        bail!("Phase 11 downsample requires even spatial dimensions");
    }

    let output_shape = Shape4D::new(
        source_shape.t,
        source_shape.z / 2,
        source_shape.y / 2,
        source_shape.x / 2,
    )?;
    let output_len = usize::try_from(output_shape.element_count()?)
        .context("Phase 11 downsample output shape does not fit usize")?;
    let mut output_values = Vec::with_capacity(output_len);
    for t in 0..output_shape.t {
        for z in 0..output_shape.z {
            for y in 0..output_shape.y {
                for x in 0..output_shape.x {
                    let mut sum = 0u64;
                    for dz in 0..2 {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let source_index = phase11_tzyx_index(
                                    source_shape,
                                    t,
                                    z * 2 + dz,
                                    y * 2 + dy,
                                    x * 2 + dx,
                                )?;
                                sum += u64::from(source_values[source_index]);
                            }
                        }
                    }
                    output_values.push((sum / 8) as u16);
                }
            }
        }
    }
    Ok((output_shape, output_values))
}

pub(super) fn phase11_sparse_occupied_brick_count(
    values: &[u16],
    shape: Shape4D,
    brick_shape: Shape4D,
) -> anyhow::Result<(u64, u64)> {
    let expected_len = usize::try_from(shape.element_count()?)
        .context("Phase 11 sparse fixture shape does not fit usize")?;
    if values.len() != expected_len {
        bail!(
            "Phase 11 sparse value length mismatch: got {}, expected {}",
            values.len(),
            expected_len
        );
    }
    let grid = shape.chunk_grid(brick_shape)?;
    let mut occupied = 0u64;
    for bt in 0..grid.t {
        for bz in 0..grid.z {
            for by in 0..grid.y {
                for bx in 0..grid.x {
                    if phase11_sparse_brick_has_nonzero(values, shape, brick_shape, bt, bz, by, bx)?
                    {
                        occupied += 1;
                    }
                }
            }
        }
    }
    Ok((occupied, grid.element_count()?))
}

fn phase11_sparse_brick_has_nonzero(
    values: &[u16],
    shape: Shape4D,
    brick_shape: Shape4D,
    bt: u64,
    bz: u64,
    by: u64,
    bx: u64,
) -> anyhow::Result<bool> {
    let t_end = ((bt + 1) * brick_shape.t).min(shape.t);
    let z_end = ((bz + 1) * brick_shape.z).min(shape.z);
    let y_end = ((by + 1) * brick_shape.y).min(shape.y);
    let x_end = ((bx + 1) * brick_shape.x).min(shape.x);
    for t in (bt * brick_shape.t)..t_end {
        for z in (bz * brick_shape.z)..z_end {
            for y in (by * brick_shape.y)..y_end {
                for x in (bx * brick_shape.x)..x_end {
                    if values[phase11_tzyx_index(shape, t, z, y, x)?] != 0 {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

fn phase11_tzyx_index(shape: Shape4D, t: u64, z: u64, y: u64, x: u64) -> anyhow::Result<usize> {
    if t >= shape.t || z >= shape.z || y >= shape.y || x >= shape.x {
        bail!("index t={t}, z={z}, y={y}, x={x} is outside shape {shape:?}");
    }
    let index = (((t * shape.z + z) * shape.y + y) * shape.x) + x;
    usize::try_from(index).context("Phase 11 sparse fixture index does not fit usize")
}
