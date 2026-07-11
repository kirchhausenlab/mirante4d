//! Product-validation facts retained only until WP-14.

use crate::ProductAutomationController;

/// Exact two-field temporary owner frozen by the WP-07B entry.
pub(crate) struct CurrentValidationRuntime {
    pub(crate) product_automation: Option<ProductAutomationController>,
    pub(crate) test_render_viewport_max_side: Option<usize>,
}

impl CurrentValidationRuntime {
    pub(crate) fn from_environment() -> Self {
        Self {
            product_automation: ProductAutomationController::from_env(),
            test_render_viewport_max_side: None,
        }
    }
}
