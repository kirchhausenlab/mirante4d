use eframe::egui;
use mirante4d_application::{ProjectGenerationId, ProjectId};

use crate::{WorkbenchUiAction, muted_label, toolbar_button};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyProjectSaveAction {
    Unavailable,
    Save,
    SaveAs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyProjectCloseView {
    open: bool,
    pending_dataset_open: bool,
    save_action: DirtyProjectSaveAction,
}

impl DirtyProjectCloseView {
    pub const fn new(
        open: bool,
        pending_dataset_open: bool,
        save_action: DirtyProjectSaveAction,
    ) -> Self {
        Self {
            open,
            pending_dataset_open,
            save_action,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecoveryCandidateView {
    generation_id: ProjectGenerationId,
    classification: String,
    origin: String,
    revision_sequence: u64,
    generation_sequence: u64,
    artifact_count: u32,
    non_regenerable_artifact_count: u32,
}

impl ProjectRecoveryCandidateView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        generation_id: ProjectGenerationId,
        classification: String,
        origin: String,
        revision_sequence: u64,
        generation_sequence: u64,
        artifact_count: u32,
        non_regenerable_artifact_count: u32,
    ) -> Self {
        Self {
            generation_id,
            classification,
            origin,
            revision_sequence,
            generation_sequence,
            artifact_count,
            non_regenerable_artifact_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecoveryView {
    review_generation: Option<ProjectGenerationId>,
    panel_open: bool,
    candidates: Vec<ProjectRecoveryCandidateView>,
    locators: Vec<ProjectId>,
    can_open_locator: bool,
}

impl ProjectRecoveryView {
    pub const fn new(
        review_generation: Option<ProjectGenerationId>,
        panel_open: bool,
        candidates: Vec<ProjectRecoveryCandidateView>,
        locators: Vec<ProjectId>,
        can_open_locator: bool,
    ) -> Self {
        Self {
            review_generation,
            panel_open,
            candidates,
            locators,
            can_open_locator,
        }
    }

    pub fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }

    pub fn has_locators(&self) -> bool {
        !self.locators.is_empty()
    }
}

pub(crate) fn show_dirty_project_close_prompt(
    ctx: &egui::Context,
    input: DirtyProjectCloseView,
    actions: &mut Vec<WorkbenchUiAction>,
) {
    if !input.open {
        return;
    }
    egui::Window::new("Unsaved Project")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(if input.pending_dataset_open {
                "Project changes have not been saved. Save or discard them before opening another dataset."
            } else {
                "Project changes have not been saved."
            });
            ui.horizontal(|ui| {
                let save_available = input.save_action != DirtyProjectSaveAction::Unavailable;
                let save_label = if input.save_action == DirtyProjectSaveAction::SaveAs {
                    "Save As"
                } else {
                    "Save"
                };
                if toolbar_button(ui, save_label, save_available).clicked() {
                    actions.push(if input.save_action == DirtyProjectSaveAction::SaveAs {
                        WorkbenchUiAction::SaveDirtyProjectAs
                    } else {
                        WorkbenchUiAction::SaveDirtyProject
                    });
                }
                if toolbar_button(ui, "Discard", true).clicked() {
                    actions.push(WorkbenchUiAction::DiscardDirtyProject);
                }
                if toolbar_button(ui, "Cancel", true).clicked() {
                    actions.push(WorkbenchUiAction::CancelDirtyProjectClose);
                }
            });
        });
}

pub(crate) fn show_project_recovery_ui(
    ctx: &egui::Context,
    input: &ProjectRecoveryView,
    actions: &mut Vec<WorkbenchUiAction>,
) {
    if let Some(automatic_newer) = input.review_generation {
        egui::Window::new("Recover autosaved project?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    "A newer autosave was found for the saved project. Recovery opens it as an unsaved branch that must be saved with Save As.",
                );
                ui.horizontal(|ui| {
                    if toolbar_button(ui, "Recover Autosave", true).clicked() {
                        actions.push(WorkbenchUiAction::RecoverReviewedAutosave(automatic_newer));
                    }
                    if toolbar_button(ui, "Open Saved Project", true).clicked() {
                        actions.push(WorkbenchUiAction::AcceptSavedProjectAfterRecoveryReview);
                    }
                });
            });
    }

    if !input.panel_open {
        return;
    }
    let mut panel_open = true;
    let mut selected = None;
    let mut selected_locator = None;
    egui::Window::new("Project Recovery")
        .open(&mut panel_open)
        .resizable(true)
        .default_width(520.0)
        .show(ctx, |ui| {
            if input.candidates.is_empty() && input.locators.is_empty() {
                ui.label("No validated recovery branches are available.");
                return;
            }
            ui.label(
                "Recovery never changes the stored project. A selected branch opens dirty and must be saved with Save As.",
            );
            ui.separator();
            if !input.locators.is_empty() {
                ui.heading("Unsaved projects from earlier launches");
                for project_id in &input.locators {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("Project {project_id}"));
                        if toolbar_button(ui, "Inspect and Recover", input.can_open_locator)
                            .on_hover_text("Validated by the project-store actor before opening")
                            .clicked()
                        {
                            selected_locator = Some(*project_id);
                        }
                    });
                }
                if !input.candidates.is_empty() {
                    ui.separator();
                }
            }
            if !input.candidates.is_empty() {
                ui.heading("Branches in the selected project");
            }
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for candidate in &input.candidates {
                        ui.group(|ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!(
                                    "{} · {} · revision {}",
                                    candidate.classification,
                                    candidate.origin,
                                    candidate.revision_sequence
                                ));
                                if toolbar_button(ui, "Open Recovery", true).clicked() {
                                    selected = Some(candidate.generation_id);
                                }
                            });
                            muted_label(
                                ui,
                                format!(
                                    "generation {} · artifacts {} ({} non-regenerable)",
                                    candidate.generation_sequence,
                                    candidate.artifact_count,
                                    candidate.non_regenerable_artifact_count
                                ),
                            );
                        });
                    }
                });
        });
    if !panel_open {
        actions.push(WorkbenchUiAction::CloseProjectRecoveryPanel);
    }
    if let Some(generation_id) = selected {
        actions.push(WorkbenchUiAction::OpenRecoveryCandidate(generation_id));
    } else if let Some(project_id) = selected_locator {
        actions.push(WorkbenchUiAction::OpenRecoveryLocator(project_id));
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::{Harness, kittest::Queryable};

    use super::*;

    struct DirtyCloseHarnessState {
        input: DirtyProjectCloseView,
        actions: Vec<WorkbenchUiAction>,
    }

    struct ProjectRecoveryHarnessState {
        input: ProjectRecoveryView,
        actions: Vec<WorkbenchUiAction>,
    }

    #[test]
    fn dirty_project_prompt_returns_ordered_choices_without_app_side_effects() {
        let mut harness = Harness::builder().build_ui_state(
            |ui, state: &mut DirtyCloseHarnessState| {
                state.actions.clear();
                show_dirty_project_close_prompt(ui.ctx(), state.input, &mut state.actions);
            },
            DirtyCloseHarnessState {
                input: DirtyProjectCloseView::new(true, true, DirtyProjectSaveAction::SaveAs),
                actions: Vec::new(),
            },
        );

        harness.get_by_label("Save As").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            vec![WorkbenchUiAction::SaveDirtyProjectAs]
        );

        harness.get_by_label("Discard").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            vec![WorkbenchUiAction::DiscardDirtyProject]
        );

        harness.get_by_label("Cancel").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            vec![WorkbenchUiAction::CancelDirtyProjectClose]
        );
    }

    #[test]
    fn project_recovery_windows_return_exact_selected_identities() {
        let reviewed_generation = generation_id(0x11);
        let mut review_harness = Harness::builder().build_ui_state(
            |ui, state: &mut ProjectRecoveryHarnessState| {
                state.actions.clear();
                show_project_recovery_ui(ui.ctx(), &state.input, &mut state.actions);
            },
            ProjectRecoveryHarnessState {
                input: ProjectRecoveryView::new(
                    Some(reviewed_generation),
                    false,
                    Vec::new(),
                    Vec::new(),
                    false,
                ),
                actions: Vec::new(),
            },
        );

        review_harness.get_by_label("Recover Autosave").click();
        review_harness.step();
        assert_eq!(
            review_harness.state().actions,
            vec![WorkbenchUiAction::RecoverReviewedAutosave(
                reviewed_generation
            )]
        );

        let project_id = ProjectId::from_bytes([7; 16]);
        let candidate_generation = generation_id(0x22);
        let candidate = ProjectRecoveryCandidateView::new(
            candidate_generation,
            "newer".to_owned(),
            "autosave_head".to_owned(),
            8,
            9,
            2,
            1,
        );
        let mut recovery_harness = Harness::builder().build_ui_state(
            |ui, state: &mut ProjectRecoveryHarnessState| {
                state.actions.clear();
                show_project_recovery_ui(ui.ctx(), &state.input, &mut state.actions);
            },
            ProjectRecoveryHarnessState {
                input: ProjectRecoveryView::new(
                    None,
                    true,
                    vec![candidate],
                    vec![project_id],
                    true,
                ),
                actions: Vec::new(),
            },
        );

        recovery_harness.get_by_label("Inspect and Recover").click();
        recovery_harness.step();
        assert_eq!(
            recovery_harness.state().actions,
            vec![WorkbenchUiAction::OpenRecoveryLocator(project_id)]
        );

        recovery_harness.get_by_label("Open Recovery").click();
        recovery_harness.step();
        assert_eq!(
            recovery_harness.state().actions,
            vec![WorkbenchUiAction::OpenRecoveryCandidate(
                candidate_generation
            )]
        );
    }

    fn generation_id(byte: u8) -> ProjectGenerationId {
        ProjectGenerationId::parse(&format!(
            "{}{}",
            ProjectGenerationId::PREFIX,
            format!("{byte:02x}").repeat(32)
        ))
        .unwrap()
    }
}
