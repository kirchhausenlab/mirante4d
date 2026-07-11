use mirante4d_core::TimeIndex;
use mirante4d_data::{DatasetHandle, VolumeRegion};

use super::test_support::{
    cpu_f32_region_summary, cpu_region_summary, write_three_brick_f32_gpu_fixture,
    write_three_brick_gpu_fixture,
};
use super::*;

fn assert_summary_timing_if_enabled(
    renderer: &GpuRenderer,
    gpu_compute_ns: Option<u64>,
    label: &str,
) {
    if renderer.adapter_diagnostics().timestamp_queries_enabled {
        assert!(
            gpu_compute_ns.is_some(),
            "{label} GPU timestamp-enabled summary must report GPU compute time"
        );
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_intensity_summary_matches_cpu_volume_summary() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let summary = renderer.summarize_u16_volume(&volume).unwrap();
    let expected_voxel_count = volume.values().len() as u64;
    let expected_nonzero_count = volume.values().iter().filter(|&&value| value != 0).count() as u64;
    let expected_min = volume.values().iter().copied().min().unwrap_or(0);
    let expected_max = volume.values().iter().copied().max().unwrap_or(0);
    let expected_sum = volume
        .values()
        .iter()
        .map(|&value| u64::from(value))
        .sum::<u64>();

    assert_eq!(summary.voxel_count, expected_voxel_count);
    assert_eq!(summary.nonzero_count, expected_nonzero_count);
    assert_eq!(summary.min, expected_min);
    assert_eq!(summary.max, expected_max);
    assert_eq!(summary.sum, expected_sum);
    assert_eq!(
        summary.mean,
        expected_sum as f64 / expected_voxel_count as f64
    );
    assert_summary_timing_if_enabled(&renderer, summary.gpu_compute_ns, "u16 full-volume");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_roi_intensity_summary_matches_cpu_region_summary() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let region = VolumeRegion::new(0, 0, 1, 2, 2, 4).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let summary = renderer.summarize_u16_region(&volume, region).unwrap();
    let mut expected = cpu_region_summary(&volume, region);
    expected.gpu_compute_ns = summary.gpu_compute_ns;

    assert_eq!(summary, expected);
    assert_summary_timing_if_enabled(&renderer, summary.gpu_compute_ns, "u16 ROI");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_float32_intensity_summary_matches_cpu_volume_summary() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let summary = renderer.summarize_f32_volume(&volume).unwrap();
    let expected = cpu_f32_region_summary(
        &volume,
        VolumeRegion::new(0, 0, 0, volume.shape.z, volume.shape.y, volume.shape.x).unwrap(),
    );

    assert_eq!(summary.voxel_count, expected.voxel_count);
    assert_eq!(summary.nonzero_count, expected.nonzero_count);
    assert_eq!(summary.min, expected.min);
    assert_eq!(summary.max, expected.max);
    assert!((summary.sum - expected.sum).abs() <= 1.0e-5);
    assert!((summary.mean - expected.mean).abs() <= 1.0e-6);
    assert_summary_timing_if_enabled(&renderer, summary.gpu_compute_ns, "f32 full-volume");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_float32_roi_intensity_summary_matches_cpu_region_summary() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();
    let region = VolumeRegion::new(0, 0, 1, 2, 2, 4).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let summary = renderer.summarize_f32_region(&volume, region).unwrap();
    let expected = cpu_f32_region_summary(&volume, region);

    assert_eq!(summary.voxel_count, expected.voxel_count);
    assert_eq!(summary.nonzero_count, expected.nonzero_count);
    assert_eq!(summary.min, expected.min);
    assert_eq!(summary.max, expected.max);
    assert!((summary.sum - expected.sum).abs() <= 1.0e-5);
    assert!((summary.mean - expected.mean).abs() <= 1.0e-6);
    assert_summary_timing_if_enabled(&renderer, summary.gpu_compute_ns, "f32 ROI");
}
