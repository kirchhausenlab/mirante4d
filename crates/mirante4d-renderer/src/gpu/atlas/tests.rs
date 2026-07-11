use mirante4d_data::{
    DenseVolumeF32, DenseVolumeU16, SpatialBrickIndex, VolumeBrickF32, VolumeBrickU16, VolumeRegion,
};
use mirante4d_domain::{GridToWorld, Shape3D, TimeIndex};
use mirante4d_format::{BrickIndex, DatasetId, LayerId};

use super::*;

#[test]
fn upload_ready_integer_brick_cache_reuses_packed_payloads() {
    let brick_shape = Shape3D::new(2, 2, 2).unwrap();
    let packed_u32_per_brick =
        IntegerAtlasDType::U16.packed_u32_per_brick(brick_shape.element_count().unwrap());
    let valid_u32_per_brick = validity_u32_per_brick(brick_shape.element_count().unwrap());
    let brick = test_u16_brick(brick_shape);
    let mut cache = UploadReadyIntegerBrickCache::new(1024);

    let first = cache
        .get_or_pack_u16(
            &brick,
            brick_shape,
            packed_u32_per_brick,
            valid_u32_per_brick,
        )
        .unwrap();
    let resident_bytes = cache.current_bytes;
    let second = cache
        .get_or_pack_u16(
            &brick,
            brick_shape,
            packed_u32_per_brick,
            valid_u32_per_brick,
        )
        .unwrap();

    assert_eq!(cache.misses, 1);
    assert_eq!(cache.hits, 1);
    assert_eq!(cache.evictions, 0);
    assert_eq!(cache.current_bytes, resident_bytes);
    assert_eq!(first.values, second.values);
    assert_eq!(first.validity_bits, second.validity_bits);
}

#[test]
fn upload_ready_integer_cache_keys_distinguish_source_subregions() {
    let brick_shape = Shape3D::new(4, 4, 4).unwrap();
    let packed_u32_per_brick =
        IntegerAtlasDType::U16.packed_u32_per_brick(brick_shape.element_count().unwrap());
    let valid_u32_per_brick = validity_u32_per_brick(brick_shape.element_count().unwrap());
    let brick_index = SpatialBrickIndex::new(0, 0, 0);
    let first_region = VolumeRegion::new(1, 0, 0, 1, 2, 2).unwrap();
    let second_region = VolumeRegion::new(2, 1, 1, 1, 2, 2).unwrap();
    let first_brick = test_u16_brick_region(brick_index, first_region, 10);
    let second_brick = test_u16_brick_region(brick_index, second_region, 100);
    let mut cache = UploadReadyIntegerBrickCache::new(4096);

    let first = cache
        .get_or_pack_u16(
            &first_brick,
            brick_shape,
            packed_u32_per_brick,
            valid_u32_per_brick,
        )
        .unwrap();
    let second = cache
        .get_or_pack_u16(
            &second_brick,
            brick_shape,
            packed_u32_per_brick,
            valid_u32_per_brick,
        )
        .unwrap();

    assert_eq!(cache.misses, 2);
    assert_eq!(cache.hits, 0);
    assert_ne!(first.values, second.values);
    assert_eq!(
        packed_u16_value(&first, local_index(brick_shape, 1, 0, 0)),
        10
    );
    assert_eq!(
        packed_u16_value(&second, local_index(brick_shape, 2, 1, 1)),
        100
    );
    assert_eq!(
        packed_u16_value(&second, local_index(brick_shape, 1, 0, 0)),
        0
    );
    assert!(packed_validity(&first, local_index(brick_shape, 1, 0, 0)));
    assert!(!packed_validity(&second, local_index(brick_shape, 1, 0, 0)));
}

#[test]
fn compact_f32_atlas_counts_actual_region_voxels_and_page_metadata() {
    let layer_id = LayerId::new("ch0").unwrap();
    let resident = ResidentBrickSetF32::new(
        layer_id.clone(),
        TimeIndex::new(0),
        Shape3D::new(3, 5, 5).unwrap(),
        GridToWorld::identity(),
        vec![
            f32_brick(
                layer_id.clone(),
                SpatialBrickIndex::new(0, 0, 0),
                VolumeRegion::new(0, 0, 0, 2, 4, 4).unwrap(),
                1.0,
            ),
            f32_brick(
                layer_id,
                SpatialBrickIndex::new(1, 1, 1),
                VolumeRegion::new(2, 4, 4, 1, 1, 1).unwrap(),
                10.0,
            ),
        ],
    );
    let brick_grid_shape = Shape3D::new(2, 2, 2).unwrap();

    assert_eq!(compact_f32_value_words(&resident).unwrap(), 33);
    assert_eq!(f32_page_table_word_count(brick_grid_shape).unwrap(), 56);
    assert_eq!(
        pack_brick_f32_compact(&resident.bricks()[1]).unwrap().len(),
        1
    );

    let mut page_table = vec![0; f32_page_table_word_count(brick_grid_shape).unwrap() as usize];
    let page_index = brick_page_index(SpatialBrickIndex::new(1, 1, 1), brick_grid_shape);
    write_f32_brick_page_table(
        &mut page_table,
        page_index,
        GpuF32BrickAllocation {
            value_offset_words: 32,
            value_words: 1,
            x_size: 1,
            y_size: 1,
            z_size: 1,
            x_start: 4,
            y_start: 4,
            z_start: 2,
        },
    );

    let base = page_index * F32_BRICK_PAGE_TABLE_WORDS as usize;
    assert_eq!(&page_table[base..base + 7], &[33, 1, 1, 1, 4, 4, 2]);
}

#[test]
fn preferred_integer_slots_allow_non_power_of_two_capacity_near_device_limit() {
    let cache = GpuBrickAtlasCache::new(2 * 1024 * 1024 * 1024);
    let limits = wgpu::Limits {
        max_buffer_size: 2 * 1024 * 1024 * 1024 - 4,
        max_storage_buffer_binding_size: 2 * 1024 * 1024 * 1024 - 4,
        ..Default::default()
    };
    let brick_shape = Shape3D::new(64, 64, 64).unwrap();
    let brick_grid_shape = Shape3D::new(96, 96, 1).unwrap();

    assert!(matches!(
        IntegerAtlasDType::U8.validate_budget(
            cache.max_bytes,
            &limits,
            brick_shape,
            brick_grid_shape,
            8192,
        ),
        Err(GpuRenderError::BufferTooLarge {
            resource: "brick atlas packed uint8 values",
            required_bytes: 2_147_483_648,
            limit_bytes: 2_147_483_644,
        })
    ));

    let selected = cache
        .preferred_integer_slot_count(
            &limits,
            IntegerAtlasDType::U8,
            brick_shape,
            brick_grid_shape,
            7251,
        )
        .unwrap();

    assert_eq!(selected, 7281);
    assert!(
        IntegerAtlasDType::U8
            .validate_budget(
                cache.max_bytes,
                &limits,
                brick_shape,
                brick_grid_shape,
                selected,
            )
            .is_ok()
    );
    assert!(matches!(
        IntegerAtlasDType::U8.validate_budget(
            cache.max_bytes,
            &limits,
            brick_shape,
            brick_grid_shape,
            selected + 1,
        ),
        Err(GpuRenderError::BudgetExceeded { .. } | GpuRenderError::BufferTooLarge { .. })
    ));
}

#[test]
fn integer_growth_reuses_existing_atlas_when_union_headroom_exceeds_budget() {
    let cache = GpuBrickAtlasCache::new(2 * 1024 * 1024 * 1024);
    let limits = wgpu::Limits {
        max_buffer_size: 2 * 1024 * 1024 * 1024 - 4,
        max_storage_buffer_binding_size: 2 * 1024 * 1024 * 1024 - 4,
        ..Default::default()
    };
    let brick_shape = Shape3D::new(64, 64, 64).unwrap();
    let brick_grid_shape = Shape3D::new(96, 96, 1).unwrap();

    let selected = cache
        .integer_growth_slot_count(
            &limits,
            IntegerAtlasGrowthRequest {
                dtype: IntegerAtlasDType::U8,
                brick_shape,
                brick_grid_shape,
                current_slot_count: 4620,
                required_slot_count: 7948,
                visible_slot_count: 3615,
            },
        )
        .unwrap();

    assert_eq!(selected, None);
}

#[test]
fn integer_growth_rejects_over_budget_current_frame() {
    let cache = GpuBrickAtlasCache::new(2 * 1024 * 1024 * 1024);
    let limits = wgpu::Limits {
        max_buffer_size: 2 * 1024 * 1024 * 1024 - 4,
        max_storage_buffer_binding_size: 2 * 1024 * 1024 * 1024 - 4,
        ..Default::default()
    };
    let brick_shape = Shape3D::new(64, 64, 64).unwrap();
    let brick_grid_shape = Shape3D::new(96, 96, 1).unwrap();

    let err = cache
        .integer_growth_slot_count(
            &limits,
            IntegerAtlasGrowthRequest {
                dtype: IntegerAtlasDType::U8,
                brick_shape,
                brick_grid_shape,
                current_slot_count: 4620,
                required_slot_count: 7948,
                visible_slot_count: 7948,
            },
        )
        .unwrap_err();

    match err {
        GpuRenderError::BudgetExceeded {
            resource: "brick atlas packed uint8 values",
            required_bytes,
            budget_bytes: 2_147_483_648,
        } => assert!(required_bytes > 2_147_483_648),
        other => panic!("expected U8 atlas budget failure, got {other}"),
    }
}

#[test]
fn current_pages_match_atlas_pages_requires_exact_same_set() {
    let page_a = SpatialBrickIndex::new(0, 0, 0);
    let page_b = SpatialBrickIndex::new(0, 0, 1);
    let page_c = SpatialBrickIndex::new(0, 1, 0);
    let current_pages = HashSet::from([page_a, page_b]);
    let page_slots = HashMap::from([(page_a, 0), (page_b, 1)]);

    assert!(current_pages_match_atlas_pages(&current_pages, &page_slots));

    let extra_page_slots = HashMap::from([(page_a, 0), (page_b, 1), (page_c, 2)]);
    assert!(!current_pages_match_atlas_pages(
        &current_pages,
        &extra_page_slots
    ));

    let different_pages = HashSet::from([page_a, page_c]);
    assert!(!current_pages_match_atlas_pages(
        &different_pages,
        &page_slots
    ));
}

#[test]
fn current_pages_match_active_pages_ignores_cached_inactive_slots() {
    let page_a = SpatialBrickIndex::new(0, 0, 0);
    let page_b = SpatialBrickIndex::new(0, 0, 1);
    let page_c = SpatialBrickIndex::new(0, 1, 0);
    let current_pages = HashSet::from([page_a, page_b]);
    let active_pages = HashSet::from([page_a, page_b]);

    assert!(current_pages_match_active_pages(
        &current_pages,
        &active_pages
    ));

    let extra_active_page = HashSet::from([page_a, page_b, page_c]);
    assert!(!current_pages_match_active_pages(
        &current_pages,
        &extra_active_page
    ));
}

#[test]
fn priority_eviction_candidates_prefer_low_priority_before_lru() {
    let page_a_old_high = SpatialBrickIndex::new(0, 0, 0);
    let page_b_new_low = SpatialBrickIndex::new(0, 0, 1);
    let page_c_current = SpatialBrickIndex::new(0, 1, 0);
    let page_d_lower_score = SpatialBrickIndex::new(0, 1, 1);
    let page_lru = VecDeque::from([
        page_a_old_high,
        page_b_new_low,
        page_c_current,
        page_d_lower_score,
    ]);
    let current_pages = HashSet::from([page_c_current]);
    let priorities = HashMap::from([
        (page_a_old_high, GpuBrickAtlasPagePriority::new(0, 100.0)),
        (page_b_new_low, GpuBrickAtlasPagePriority::new(3, 100.0)),
        (page_c_current, GpuBrickAtlasPagePriority::new(3, -100.0)),
        (page_d_lower_score, GpuBrickAtlasPagePriority::new(3, -10.0)),
    ]);

    let candidates = prioritized_eviction_candidates(&page_lru, &current_pages, &priorities);

    assert_eq!(
        candidates,
        vec![page_d_lower_score, page_b_new_low, page_a_old_high]
    );
}

fn test_u16_brick(shape: Shape3D) -> VolumeBrickU16 {
    let region = VolumeRegion {
        z_start: 0,
        y_start: 0,
        x_start: 0,
        z_size: shape.z(),
        y_size: shape.y(),
        x_size: shape.x(),
    };
    test_u16_brick_region(SpatialBrickIndex::new(0, 0, 0), region, 0)
}

fn test_u16_brick_region(
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    first_value: u16,
) -> VolumeBrickU16 {
    let shape = region.shape().unwrap();
    let dataset_id = DatasetId::new("upload-ready-cache-test").unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let values = (0..shape.element_count().unwrap())
        .map(|value| first_value + value as u16)
        .collect();
    let volume = DenseVolumeU16::new(
        dataset_id,
        layer_id,
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    VolumeBrickU16 {
        scale_level: 0,
        brick_index,
        chunk_index: BrickIndex {
            t: 0,
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        },
        region,
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: f64::from(first_value),
        max: f64::from(first_value) + shape.element_count().unwrap() as f64 - 1.0,
        volume,
    }
}

fn local_index(shape: Shape3D, z: u64, y: u64, x: u64) -> usize {
    ((z * shape.y() + y) * shape.x() + x) as usize
}

fn packed_u16_value(packed: &PackedIntegerBrick, index: usize) -> u16 {
    let word = packed.values[index / 2];
    let shift = ((index % 2) as u32) * 16;
    ((word >> shift) & 0xffff) as u16
}

fn packed_validity(packed: &PackedIntegerBrick, index: usize) -> bool {
    packed.validity_bits[index / 32] & (1u32 << (index % 32)) != 0
}

fn f32_brick(
    layer_id: LayerId,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    first_value: f32,
) -> VolumeBrickF32 {
    let shape = region.shape().unwrap();
    let values = (0..shape.element_count().unwrap())
        .map(|index| first_value + index as f32)
        .collect::<Vec<_>>();
    let volume = DenseVolumeF32::new(
        DatasetId::new("compact-f32-atlas-test").unwrap(),
        layer_id,
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    VolumeBrickF32 {
        scale_level: 0,
        brick_index,
        chunk_index: BrickIndex {
            t: 0,
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        },
        region,
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: f64::from(first_value),
        max: f64::from(first_value + shape.element_count().unwrap() as f32 - 1.0),
        volume,
    }
}
