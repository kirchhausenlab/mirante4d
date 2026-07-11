use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::PathBuf,
    sync::Arc,
};

use mirante4d_app::{
    AppState, ChannelRenderState, MiranteWorkbenchApp, RenderMode, collect_startup_diagnostics,
    default_app_preferences_for_system, default_log_path, default_preferences_path,
    load_app_preferences, open_dataset_and_render_first_frame,
    open_dataset_with_preferences_and_render_first_frame, render_first_streamed_frame_for_smoke,
    render_playback_steps_for_smoke,
};
use mirante4d_renderer::gpu::{
    GPU_ADAPTER_ENV, GpuRenderer, adapter_info_matches_name, adapter_info_summary,
    adapter_preference_score, renderer_device_descriptor, renderer_required_limits_for_adapter,
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
        let mut state = open_dataset_and_render_first_frame(&dataset)?;
        if let Some(raw_modes) = std::env::var("MIRANTE4D_APP_SMOKE_CHANNEL_RENDER_MODES")
            .ok()
            .filter(|raw| !raw.trim().is_empty())
        {
            apply_channel_render_modes_for_launch(
                &mut state,
                "MIRANTE4D_APP_SMOKE_CHANNEL_RENDER_MODES",
                &raw_modes,
            )?;
        }
        let smoke_render_mode = std::env::var("MIRANTE4D_APP_SMOKE_RENDER_MODE")
            .ok()
            .map(|raw| parse_smoke_render_mode(&raw))
            .transpose()?
            .unwrap_or(state.active_render_mode);
        let smoke_render_modes = if let Some(raw_sequence) =
            std::env::var("MIRANTE4D_APP_SMOKE_RENDER_MODE_SEQUENCE")
                .ok()
                .filter(|raw| !raw.trim().is_empty())
        {
            parse_smoke_render_mode_sequence(&raw_sequence)?
        } else {
            vec![smoke_render_mode]
        };
        let disable_gpu_smoke = std::env::var("MIRANTE4D_APP_SMOKE_DISABLE_GPU")
            .ok()
            .is_some_and(|raw| matches!(raw.trim(), "1" | "true" | "TRUE" | "yes" | "YES"));
        let gpu_renderer = if disable_gpu_smoke {
            tracing::info!("GPU renderer disabled by MIRANTE4D_APP_SMOKE_DISABLE_GPU");
            None
        } else {
            match GpuRenderer::new_blocking() {
                Ok(renderer) => {
                    let adapter = renderer.adapter_diagnostics();
                    let summary = format!(
                        "{} {} {} driver={} {}",
                        adapter.backend,
                        adapter.device_type,
                        adapter.name,
                        adapter.driver,
                        adapter.driver_info
                    );
                    tracing::info!(gpu_adapter = %summary, "initialized GPU renderer");
                    Some((renderer, summary))
                }
                Err(err) => {
                    tracing::warn!(error = %err, "GPU renderer unavailable during app smoke");
                    None
                }
            }
        };
        let smoke_timeout = std::env::var("MIRANTE4D_APP_SMOKE_TIMEOUT_SECS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(30);
        for mode in &smoke_render_modes {
            state.active_render_mode = *mode;
            sync_active_render_state_for_launch(&mut state);
            render_first_streamed_frame_for_smoke(
                &mut state,
                gpu_renderer.as_ref().map(|(renderer, _summary)| renderer),
                std::time::Duration::from_secs(smoke_timeout),
            )?;
            tracing::info!(
                render_mode = render_mode_label(*mode),
                displayed_scale = state.frame_fidelity.displayed_scale_level,
                target_scale = state.frame_fidelity.target_scale_level,
                fidelity_reason = ?state.frame_fidelity.reason,
                "completed app smoke render step"
            );
        }
        let smoke_render_mode = *smoke_render_modes
            .last()
            .expect("smoke render mode sequence is non-empty");
        tracing::info!(
            dataset = state.dataset_name,
            layer_count = state.layer_count,
            width = state.frame.width,
            height = state.frame.height,
            nonzero_pixels = state.diagnostics.nonzero_pixels,
            max_value = state.diagnostics.max_value,
            target_scale = state.frame_fidelity.target_scale_level,
            displayed_scale = state.frame_fidelity.displayed_scale_level,
            fidelity_reason = ?state.frame_fidelity.reason,
            channel_modes = %format_channel_render_modes(&state),
            "rendered first frame"
        );
        let displayed_scale = state
            .frame_fidelity
            .displayed_scale_level
            .map(|scale| format!("s{scale}"))
            .unwrap_or_else(|| "none".to_owned());
        let dvr_stats = dvr_rgba_stats(&state.frame);
        if let Some(stats) = dvr_stats {
            println!(
                "Mirante4D {APP_VERSION} opened {}: {}x{} {}, channels [{}], {} nonzero pixels, max {}, DVR RGBA {} nontransparent pixels, max alpha {:.6}, max rgb {:.6}, displayed {}, target s{}, {:?}",
                state.dataset_name,
                state.frame.width,
                state.frame.height,
                render_mode_label(smoke_render_mode),
                format_channel_render_modes(&state),
                state.diagnostics.nonzero_pixels,
                state.diagnostics.max_value,
                stats.nontransparent_pixels,
                stats.max_alpha,
                stats.max_rgb,
                displayed_scale,
                state.frame_fidelity.target_scale_level,
                state.frame_fidelity.reason,
            );
        } else {
            println!(
                "Mirante4D {APP_VERSION} opened {}: {}x{} {}, channels [{}], {} nonzero pixels, max {}, displayed {}, target s{}, {:?}",
                state.dataset_name,
                state.frame.width,
                state.frame.height,
                render_mode_label(smoke_render_mode),
                format_channel_render_modes(&state),
                state.diagnostics.nonzero_pixels,
                state.diagnostics.max_value,
                displayed_scale,
                state.frame_fidelity.target_scale_level,
                state.frame_fidelity.reason
            );
        }
        let playback_steps = std::env::var("MIRANTE4D_APP_SMOKE_PLAYBACK_STEPS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(0);
        if playback_steps > 0 {
            let playback_frames = render_playback_steps_for_smoke(
                &mut state,
                gpu_renderer.as_ref().map(|(renderer, _summary)| renderer),
                playback_steps,
                std::time::Duration::from_secs(smoke_timeout),
            )?;
            let visited = playback_frames
                .iter()
                .map(|frame| format!("t{}", frame.timepoint + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let max_step_ms = playback_frames
                .iter()
                .map(|frame| frame.elapsed_ms)
                .fold(0.0f64, f64::max);
            let min_nonzero = playback_frames
                .iter()
                .map(|frame| frame.nonzero_pixels)
                .min()
                .unwrap_or(0);
            let mut displayed_scale_counts = BTreeMap::new();
            let mut target_scale_counts = BTreeMap::new();
            for frame in &playback_frames {
                *displayed_scale_counts
                    .entry(frame.displayed_scale_level)
                    .or_insert(0usize) += 1;
                *target_scale_counts
                    .entry(frame.target_scale_level)
                    .or_insert(0usize) += 1;
            }
            println!(
                "Mirante4D playback smoke: {} step(s), visited [{}], max step {:.3} ms, min nonzero pixels {}, displayed scales [{}], target scales [{}]",
                playback_frames.len(),
                visited,
                max_step_ms,
                min_nonzero,
                format_scale_counts(&displayed_scale_counts),
                format_scale_counts(&target_scale_counts)
            );
        }
        println!(
            "{}",
            state.startup_diagnostics.summary_text(
                Some(&state.dataset_path),
                gpu_renderer
                    .as_ref()
                    .map(|(_renderer, summary)| summary.as_str())
            )
        );
        if let Some((_renderer, adapter_summary)) = gpu_renderer {
            println!("GPU adapter: {adapter_summary}");
        }
        return Ok(());
    }

    let preferences_path = default_preferences_path();
    let preferences = match preferences_path.as_deref() {
        Some(path) if path.exists() => load_app_preferences(path)?,
        Some(_) | None => default_app_preferences_for_system(),
    };

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
    let mut state = open_dataset_with_preferences_and_render_first_frame(&dataset, &preferences)?;
    apply_dev_launch_overrides(&mut state)?;

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
        Box::new(move |cc| {
            Ok(Box::new(MiranteWorkbenchApp::new_with_preferences(
                cc,
                state,
                preferences,
                preferences_path,
            )))
        }),
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

fn parse_smoke_render_mode(raw: &str) -> anyhow::Result<RenderMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mip" => Ok(RenderMode::Mip),
        "dvr" => Ok(RenderMode::Dvr),
        "iso" | "isosurface" => Ok(RenderMode::Isosurface),
        other => anyhow::bail!(
            "unsupported MIRANTE4D_APP_SMOKE_RENDER_MODE {other:?}; expected mip, dvr, or iso"
        ),
    }
}

fn apply_dev_launch_overrides(state: &mut AppState) -> anyhow::Result<()> {
    if let Some(raw_mode) = std::env::var("MIRANTE4D_DEV_RENDER_MODE")
        .ok()
        .filter(|raw| !raw.trim().is_empty())
    {
        state.active_render_mode = parse_smoke_render_mode(&raw_mode)?;
        sync_active_render_state_for_launch(state);
    }
    if let Some(raw_modes) = std::env::var("MIRANTE4D_DEV_CHANNEL_RENDER_MODES")
        .ok()
        .filter(|raw| !raw.trim().is_empty())
    {
        apply_channel_render_modes_for_launch(
            state,
            "MIRANTE4D_DEV_CHANNEL_RENDER_MODES",
            &raw_modes,
        )?;
    }
    Ok(())
}

fn apply_channel_render_modes_for_launch(
    state: &mut AppState,
    env_name: &'static str,
    raw_modes: &str,
) -> anyhow::Result<()> {
    let entries = raw_modes
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        anyhow::bail!("{env_name} must contain at least one channel render mode");
    }
    for (position, entry) in entries.iter().enumerate() {
        let (layer_index, raw_mode) = if let Some((raw_layer, raw_mode)) = entry.split_once(':') {
            let raw_layer = raw_layer.trim();
            let layer_index = state
                .layers
                .iter()
                .position(|layer| layer.id == raw_layer)
                .ok_or_else(|| {
                    anyhow::anyhow!("{env_name} references unknown layer id {raw_layer:?}")
                })?;
            (layer_index, raw_mode.trim())
        } else {
            (position, *entry)
        };
        if layer_index >= state.layers.len() {
            anyhow::bail!(
                "{env_name} positional entry {} has no matching layer; dataset has {} layer(s)",
                position + 1,
                state.layers.len()
            );
        }
        let mode = parse_smoke_render_mode(raw_mode)?;
        apply_layer_render_mode_for_launch(state, layer_index, mode);
    }
    Ok(())
}

fn apply_layer_render_mode_for_launch(state: &mut AppState, layer_index: usize, mode: RenderMode) {
    if layer_index == state.active_layer_index {
        state.active_render_mode = mode;
        sync_active_render_state_for_launch(state);
        return;
    }
    let current = state.layers[layer_index].render_state;
    let dvr_opacity_transfer = state.layers[layer_index].dvr_opacity_transfer;
    let render_state = ChannelRenderState::for_mode(
        mode,
        current.sampling_policy(),
        current.iso_shading_policy(),
        current.iso_display_level(),
        current.dvr_opacity_transfer(dvr_opacity_transfer),
        current.dvr_density_scale(),
    );
    state.layers[layer_index].render_state = render_state;
    if let ChannelRenderState::Dvr(parameters) = render_state {
        state.layers[layer_index].dvr_opacity_transfer = parameters.opacity_transfer;
    }
}

fn sync_active_render_state_for_launch(state: &mut AppState) {
    if let Some(layer) = state.layers.get_mut(state.active_layer_index) {
        layer.render_state = ChannelRenderState::for_mode(
            state.active_render_mode,
            state.render_sampling_policy,
            state.render_iso_shading_policy,
            state.iso_display_level,
            state.active_dvr_opacity_transfer,
            state.dvr_density_scale,
        );
        if let ChannelRenderState::Dvr(parameters) = layer.render_state {
            layer.dvr_opacity_transfer = parameters.opacity_transfer;
        }
    }
}

fn parse_smoke_render_mode_sequence(raw: &str) -> anyhow::Result<Vec<RenderMode>> {
    let modes = raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_smoke_render_mode)
        .collect::<anyhow::Result<Vec<_>>>()?;
    if modes.is_empty() {
        anyhow::bail!("MIRANTE4D_APP_SMOKE_RENDER_MODE_SEQUENCE must contain at least one mode");
    }
    Ok(modes)
}

fn format_channel_render_modes(state: &AppState) -> String {
    state
        .layers
        .iter()
        .enumerate()
        .filter(|(_, layer)| layer.display.visible)
        .map(|(index, layer)| {
            let mode = if index == state.active_layer_index {
                state.active_render_mode
            } else {
                layer.render_state.mode()
            };
            format!("{}={}", layer.id, render_mode_label(mode))
        })
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

#[derive(Debug, Clone, Copy)]
struct DvrRgbaStats {
    nontransparent_pixels: u64,
    max_alpha: f32,
    max_rgb: f32,
}

fn dvr_rgba_stats(frame: &mirante4d_renderer::MipImageU16) -> Option<DvrRgbaStats> {
    let dvr = frame.dvr_rgba()?;
    let mut nontransparent_pixels = 0_u64;
    let mut max_alpha = 0.0_f32;
    let mut max_rgb = 0.0_f32;
    for rgba in dvr.premultiplied_rgba() {
        max_alpha = max_alpha.max(rgba[3]);
        max_rgb = max_rgb.max(rgba[0]).max(rgba[1]).max(rgba[2]);
        if rgba[3] > 0.0 || rgba[0] > 0.0 || rgba[1] > 0.0 || rgba[2] > 0.0 {
            nontransparent_pixels += 1;
        }
    }
    Some(DvrRgbaStats {
        nontransparent_pixels,
        max_alpha,
        max_rgb,
    })
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
