//! Current egui-local facts retained only until WP-09C.

use mirante4d_settings::ResourcePolicy;

use crate::{
    AnalysisPlotViewRange, AnalysisTableSort, ViewerToolState, ViewportHover,
    viewport::ViewportOrbitDragState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourcePolicyDraft {
    pub(crate) cpu_dataset_budget_bytes: u64,
    pub(crate) gpu_budget_bytes: u64,
}

impl From<ResourcePolicy> for ResourcePolicyDraft {
    fn from(policy: ResourcePolicy) -> Self {
        Self {
            cpu_dataset_budget_bytes: policy.cpu_dataset_budget_bytes(),
            gpu_budget_bytes: policy.gpu_budget_bytes(),
        }
    }
}

/// Temporary owner for egui-local interaction facts until WP-09C.
pub(crate) struct CurrentUiRuntime {
    pub(crate) viewport_orbit_drag: Option<ViewportOrbitDragState>,
    pub(crate) analysis_plot_view: Option<AnalysisPlotViewRange>,
    pub(crate) analysis_filter: String,
    pub(crate) analysis_sort: Option<AnalysisTableSort>,
    pub(crate) viewer_tools: ViewerToolState,
    pub(crate) hovered_pixel: Option<ViewportHover>,
    pub(crate) hovered_source_readout: Option<String>,
    pub(crate) close_prompt_open: bool,
    pub(crate) allow_close_without_prompt: bool,
    pub(crate) settings_runtime_draft: ResourcePolicyDraft,
    pub(crate) analysis_workspace_open: bool,
}

impl CurrentUiRuntime {
    pub(crate) fn new(resource_policy: ResourcePolicy) -> Self {
        Self {
            viewport_orbit_drag: None,
            analysis_plot_view: None,
            analysis_filter: String::new(),
            analysis_sort: None,
            viewer_tools: ViewerToolState::default(),
            hovered_pixel: None,
            hovered_source_readout: None,
            close_prompt_open: false,
            allow_close_without_prompt: false,
            settings_runtime_draft: resource_policy.into(),
            analysis_workspace_open: false,
        }
    }
}
