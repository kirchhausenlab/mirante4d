use std::{fs, path::PathBuf, time::Instant};

use anyhow::Context;
use mirante4d_core::{DEFAULT_PRESENTATION_VIEWPORT_POINTS, TimeIndex};
use mirante4d_data::{DatasetHandle, SpatialBrickIndex};
use mirante4d_renderer::{CameraRenderMode, RenderViewport, render_camera};

use crate::benchmark_camera_for_volume;
use crate::fixtures::generate_fixture;

pub(crate) fn bench_smoke() -> anyhow::Result<PathBuf> {
    let started = Instant::now();
    let fixture_started = Instant::now();
    let dataset_path = generate_fixture("time-multichannel-u16-8cube-3t-2c")?;
    let fixture_ms = fixture_started.elapsed().as_secs_f64() * 1000.0;

    let open_started = Instant::now();
    let dataset = DatasetHandle::open(&dataset_path)?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;

    let layer_id = dataset.first_layer_id()?;
    let read_started = Instant::now();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0))?;
    let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
    let brick_read_started = Instant::now();
    let brick = dataset.read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))?;
    let brick_read_ms = brick_read_started.elapsed().as_secs_f64() * 1000.0;

    let camera =
        benchmark_camera_for_volume(&volume).to_camera_state(DEFAULT_PRESENTATION_VIEWPORT_POINTS);
    let viewport = RenderViewport::new(volume.shape.x, volume.shape.y)?;
    let render_started = Instant::now();
    let (_frame, diagnostics) = render_camera(&volume, camera, viewport, CameraRenderMode::Mip)?;
    let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
    let stats = dataset.stats()?;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;

    let output_root = PathBuf::from("target/mirante4d/benchmarks");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let output_path = output_root.join("bench-smoke.json");
    fs::write(
        &output_path,
        format!(
            concat!(
                "{{\n",
                "  \"benchmark\": \"bench-smoke\",\n",
                "  \"dataset\": \"{}\",\n",
                "  \"shape\": {{ \"z\": {}, \"y\": {}, \"x\": {} }},\n",
                "  \"timings_ms\": {{\n",
                "    \"fixture\": {:.6},\n",
                "    \"open\": {:.6},\n",
                "    \"read_timepoint\": {:.6},\n",
                "    \"read_brick\": {:.6},\n",
                "    \"cpu_camera_mip\": {:.6},\n",
                "    \"total\": {:.6}\n",
                "  }},\n",
                "  \"data_stats\": {{\n",
                "    \"subset_reads\": {},\n",
                "    \"decoded_values\": {},\n",
                "    \"volume_cache_hits\": {},\n",
                "    \"volume_cache_misses\": {},\n",
                "    \"brick_cache_hits\": {},\n",
                "    \"brick_cache_misses\": {},\n",
                "    \"brick_reads\": {},\n",
                "    \"decoded_brick_values\": {}\n",
                "  }},\n",
                "  \"brick\": {{\n",
                "    \"z\": {}, \"y\": {}, \"x\": {},\n",
                "    \"values\": {},\n",
                "    \"occupied\": {}\n",
                "  }},\n",
                "  \"frame\": {{\n",
                "    \"output_pixels\": {},\n",
                "    \"nonzero_pixels\": {},\n",
                "    \"max_value\": {}\n",
                "  }}\n",
                "}}\n"
            ),
            dataset_path.display(),
            volume.shape.z,
            volume.shape.y,
            volume.shape.x,
            fixture_ms,
            open_ms,
            read_ms,
            brick_read_ms,
            render_ms,
            total_ms,
            stats.subset_reads,
            stats.decoded_values,
            stats.volume_cache_hits,
            stats.volume_cache_misses,
            stats.brick_cache_hits,
            stats.brick_cache_misses,
            stats.brick_reads,
            stats.decoded_brick_values,
            brick.volume.shape.z,
            brick.volume.shape.y,
            brick.volume.shape.x,
            brick.values().len(),
            brick.occupied,
            diagnostics.output_pixels,
            diagnostics.nonzero_pixels,
            diagnostics.max_value
        ),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}
