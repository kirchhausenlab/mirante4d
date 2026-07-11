use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::PathBuf,
    sync::Arc,
};

use mirante4d_app::{
    AppSmokeOptions, MiranteWorkbenchApp, collect_startup_diagnostics, default_log_path,
    run_headless_smoke,
};
use mirante4d_domain::RenderMode;
use mirante4d_renderer::gpu::{
    GPU_ADAPTER_ENV, adapter_info_matches_name, adapter_info_summary, adapter_preference_score,
    renderer_device_descriptor, renderer_required_limits_for_adapter,
};

fn main() -> anyhow::Result<()> {
    const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

    init_tracing();

    let startup_diagnostics = collect_startup_diagnostics();
    tracing::info!(
        diagnostics_format = %startup_diagnostics.format,
        app_version = %startup_diagnostics.app_version,
        target_os = %startup_diagnostics.target_os,
        target_arch = %startup_diagnostics.target_arch,
        target_family = %startup_diagnostics.target_family,
        logs_path = ?startup_diagnostics.logs_path,
        "startup diagnostics"
    );

    if std::env::var_os("MIRANTE4D_APP_SMOKE").is_some() {
        let dataset = std::env::var_os("MIRANTE4D_DEV_DATASET")
            .map(PathBuf::from)
            .ok_or_else(|| {
                anyhow::anyhow!("MIRANTE4D_DEV_DATASET must point to a native .m4d package")
            })?;
        let disable_gpu_smoke = std::env::var("MIRANTE4D_APP_SMOKE_DISABLE_GPU")
            .ok()
            .is_some_and(|raw| matches!(raw.trim(), "1" | "true" | "TRUE" | "yes" | "YES"));
        let smoke_timeout = std::env::var("MIRANTE4D_APP_SMOKE_TIMEOUT_SECS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(30);
        let playback_steps = std::env::var("MIRANTE4D_APP_SMOKE_PLAYBACK_STEPS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(0);
        let report = run_headless_smoke(
            &dataset,
            AppSmokeOptions {
                disable_gpu: disable_gpu_smoke,
                playback_steps,
                timeout: std::time::Duration::from_secs(smoke_timeout),
            },
        )?;
        let displayed_scale = report
            .displayed_scale_level
            .map(|scale| format!("s{scale}"))
            .unwrap_or_else(|| "none".to_owned());
        println!(
            "Mirante4D {APP_VERSION} opened {}: {}x{} {}, {} layer(s), {} nonzero pixels, max {}, displayed {}, target s{}",
            report.dataset_label,
            report.frame_width,
            report.frame_height,
            render_mode_label(report.render_mode),
            report.layer_count,
            report.nonzero_pixels,
            report.max_value,
            displayed_scale,
            report.target_scale_level,
        );
        if !report.playback.is_empty() {
            let visited = report
                .playback
                .iter()
                .map(|frame| format!("t{}", frame.timepoint + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let max_step_ms = report
                .playback
                .iter()
                .map(|frame| frame.elapsed_ms)
                .fold(0.0f64, f64::max);
            let min_nonzero = report
                .playback
                .iter()
                .map(|frame| frame.nonzero_pixels)
                .min()
                .unwrap_or(0);
            let mut displayed_scale_counts = BTreeMap::new();
            let mut target_scale_counts = BTreeMap::new();
            for frame in &report.playback {
                *displayed_scale_counts
                    .entry(frame.displayed_scale_level)
                    .or_insert(0usize) += 1;
                *target_scale_counts
                    .entry(frame.target_scale_level)
                    .or_insert(0usize) += 1;
            }
            println!(
                "Mirante4D playback smoke: {} step(s), visited [{}], max step {:.3} ms, min nonzero pixels {}, displayed scales [{}], target scales [{}]",
                report.playback.len(),
                visited,
                max_step_ms,
                min_nonzero,
                format_scale_counts(&displayed_scale_counts),
                format_scale_counts(&target_scale_counts)
            );
        }
        if let Some(adapter_summary) = report.gpu_adapter_summary {
            println!("GPU adapter: {adapter_summary}");
        }
        return Ok(());
    }

    let Some(dataset) = std::env::var_os("MIRANTE4D_DEV_DATASET")
        .map(PathBuf::from)
        .or_else(|| {
            rfd::FileDialog::new()
                .set_title("Open Mirante4D dataset package")
                .pick_folder()
        })
    else {
        return Ok(());
    };
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Mirante4D")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };

    eframe::run_native(
        "Mirante4D",
        native_options,
        Box::new(move |cc| Ok(Box::new(MiranteWorkbenchApp::open_dataset(cc, &dataset)?))),
    )
    .map_err(|err| anyhow::anyhow!("failed to launch native window: {err}"))
}

fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::from_default_env();
    if let Some(path) = default_log_path()
        && path
            .parent()
            .map(|parent| fs::create_dir_all(parent).is_ok())
            .unwrap_or(true)
        && let Ok(file) = OpenOptions::new().create(true).append(true).open(&path)
    {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::sync::Mutex::new(file))
            .init();
        return;
    }

    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

fn format_scale_counts(counts: &BTreeMap<u32, usize>) -> String {
    counts
        .iter()
        .map(|(scale, count)| format!("s{scale}:{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_mode_label(mode: RenderMode) -> &'static str {
    match mode {
        RenderMode::Mip => "MIP",
        RenderMode::Isosurface => "ISO",
        RenderMode::Dvr => "DVR",
    }
}

fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(create_new) = &mut options.wgpu_setup {
        create_new.power_preference = eframe::wgpu::PowerPreference::HighPerformance;
        create_new.native_adapter_selector = Some(Arc::new(|adapters, surface| {
            select_window_adapter(adapters, surface)
        }));
        create_new.device_descriptor = Arc::new(|adapter| {
            renderer_device_descriptor(adapter, "mirante4d-eframe-wgpu-device")
                .expect("selected window adapter must satisfy Mirante4D renderer limits")
        });
    }
    options
}

fn select_window_adapter(
    adapters: &[eframe::wgpu::Adapter],
    compatible_surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    let requested = std::env::var(GPU_ADAPTER_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let mut available = Vec::new();
    let mut selected = None;

    for adapter in adapters {
        let info = adapter.get_info();
        available.push(adapter_info_summary(&info));
        if let Some(surface) = compatible_surface
            && !adapter.is_surface_supported(surface)
        {
            continue;
        }
        if info.device_type == eframe::wgpu::DeviceType::Cpu {
            continue;
        }
        if let Some(requested) = &requested
            && !adapter_info_matches_name(&info, requested)
        {
            continue;
        }
        if let Err(err) = renderer_required_limits_for_adapter(adapter) {
            available.push(format!(
                "{} rejected for renderer limits: {err}",
                adapter_info_summary(&info)
            ));
            continue;
        }
        let score = adapter_preference_score(&info);
        if selected
            .as_ref()
            .is_none_or(|(best_score, _adapter, _summary)| score > *best_score)
        {
            selected = Some((score, adapter.clone(), adapter_info_summary(&info)));
        }
    }

    if let Some((_score, adapter, summary)) = selected {
        tracing::info!(adapter = %summary, "selected wgpu window adapter");
        return Ok(adapter);
    }

    let requested = requested
        .map(|value| format!(" matching {GPU_ADAPTER_ENV}={value:?}"))
        .unwrap_or_default();
    Err(format!(
        "no usable non-CPU window adapter{requested}; available adapters: {}",
        available.join("; ")
    ))
}
