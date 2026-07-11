use super::*;

impl MiranteWorkbenchApp {
    pub(super) fn project_dirty(&self) -> bool {
        ProjectDirtySnapshot::from_state(&self.state) != self.clean_project_snapshot
    }

    pub(super) fn mark_project_clean(&mut self) {
        self.clean_project_snapshot = ProjectDirtySnapshot::from_state(&self.state);
        self.close_prompt_open = false;
        self.allow_close_without_prompt = false;
    }

    pub(super) fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.allow_close_without_prompt || !self.project_dirty() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.close_prompt_open = true;
    }

    pub(super) fn show_dirty_project_close_prompt(&mut self, ctx: &egui::Context) {
        if !self.close_prompt_open {
            return;
        }
        egui::Window::new("Unsaved Project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Project changes have not been saved.");
                ui.horizontal(|ui| {
                    if ui_kit::toolbar_button(ui, "Save", true).clicked()
                        && self.save_project_for_close()
                    {
                        self.allow_close_without_prompt = true;
                        self.close_prompt_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui_kit::toolbar_button(ui, "Discard", true).clicked() {
                        self.allow_close_without_prompt = true;
                        self.close_prompt_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui_kit::toolbar_button(ui, "Cancel", true).clicked() {
                        self.close_prompt_open = false;
                        self.allow_close_without_prompt = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    }
                });
            });
    }

    pub(super) fn replace_state_from_dataset_path(
        &mut self,
        path: PathBuf,
        ctx: Option<&egui::Context>,
    ) -> anyhow::Result<()> {
        let mut state =
            open_dataset_with_preferences_and_render_first_frame(&path, &self.preferences)?;
        if let Err(err) = rerender_state_with_backend(&mut state, self.gpu_renderer.as_deref()) {
            state.last_render_error = Some(err.to_string());
        }
        state.last_workflow_message = Some(format!("Opened {}", path.display()));
        self.cancel_runtime_brick_tickets();
        self.brick_read_pool = create_brick_read_pool(&state);
        self.cross_section_read_pool = create_cross_section_read_pool(&state);
        self.pending_tiff_import = None;
        self.tiff_import_setup_task = None;
        self.tiff_import_setup_error = None;
        self.analysis_task = None;
        self.playback = PlaybackState::default();
        self.state = state;
        self.current_project_path = None;
        self.mark_project_clean();
        self.clear_gpu_display_frame();
        self.request_opened_state_visible_work(ctx);
        self.texture = None;
        Ok(())
    }

    pub(super) fn open_native_from_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open Mirante4D dataset package")
            .pick_folder()
        else {
            return;
        };
        if let Err(err) = self.replace_state_from_dataset_path(path, Some(ctx)) {
            self.state.last_render_error = Some(err.to_string());
            tracing::error!(error = %err, "failed to open native dataset from dialog");
        }
    }

    pub(super) fn open_session_from_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open Mirante4D project package")
            .pick_folder()
        else {
            return;
        };
        match read_session_file(&path) {
            Ok(session) => self.open_loaded_session_from_path(path, session, ctx),
            Err(err) => {
                self.state.last_render_error = Some(err.to_string());
                tracing::error!(error = %err, "failed to open session");
            }
        }
    }

    pub(super) fn open_loaded_session_from_path(
        &mut self,
        path: PathBuf,
        session: AppSession,
        ctx: &egui::Context,
    ) {
        match open_state_from_session_with_preferences(
            &session,
            self.gpu_renderer.as_deref(),
            &self.preferences,
        ) {
            Ok(mut state) => {
                state.last_workflow_message = Some(format!("Opened project {}", path.display()));
                let clean_snapshot = ProjectDirtySnapshot::from_state(&state);
                self.install_opened_project_state(path, state, clean_snapshot, Some(ctx));
            }
            Err(err) if !session.dataset.path.is_dir() => {
                self.locate_missing_project_dataset_from_dialog(path, session, err, ctx);
            }
            Err(err) => {
                self.state.last_render_error = Some(err.to_string());
                tracing::error!(error = %err, "failed to open session");
            }
        }
    }

    pub(super) fn locate_missing_project_dataset_from_dialog(
        &mut self,
        project_path: PathBuf,
        session: AppSession,
        open_error: anyhow::Error,
        ctx: &egui::Context,
    ) {
        let Some(dataset_path) = rfd::FileDialog::new()
            .set_title("Locate Mirante4D dataset package")
            .pick_folder()
        else {
            self.state.last_render_error = Some(format!(
                "Project dataset was not found at {}; locate the referenced .m4d package to open this project. Original error: {open_error}",
                session.dataset.path.display()
            ));
            return;
        };
        match open_state_from_session_with_relocated_dataset(
            &session,
            &dataset_path,
            self.gpu_renderer.as_deref(),
            &self.preferences,
        ) {
            Ok(mut state) => {
                state.last_workflow_message = Some(format!(
                    "Opened project {} with relocated dataset {}",
                    project_path.display(),
                    dataset_path.display()
                ));
                let clean_snapshot = ProjectDirtySnapshot::from_session_and_state(session, &state);
                self.install_opened_project_state(project_path, state, clean_snapshot, Some(ctx));
            }
            Err(err) => {
                self.state.last_render_error = Some(format!(
                    "Selected dataset {} does not match this project: {err}",
                    dataset_path.display()
                ));
                tracing::error!(error = %err, "failed to open relocated project dataset");
            }
        }
    }

    pub(super) fn install_opened_project_state(
        &mut self,
        path: PathBuf,
        state: AppState,
        clean_snapshot: ProjectDirtySnapshot,
        ctx: Option<&egui::Context>,
    ) {
        self.cancel_runtime_brick_tickets();
        self.brick_read_pool = create_brick_read_pool(&state);
        self.cross_section_read_pool = create_cross_section_read_pool(&state);
        self.pending_tiff_import = None;
        self.tiff_import_setup_task = None;
        self.tiff_import_setup_error = None;
        self.analysis_task = None;
        self.playback = PlaybackState::default();
        self.state = state;
        self.current_project_path = Some(path);
        self.clean_project_snapshot = clean_snapshot;
        self.close_prompt_open = false;
        self.allow_close_without_prompt = false;
        self.clear_gpu_display_frame();
        self.request_opened_state_visible_work(ctx);
        self.texture = None;
    }

    pub(super) fn request_opened_state_visible_work(&mut self, _ctx: Option<&egui::Context>) {
        self.request_visible_bricks();
    }

    pub(super) fn save_current_project(&mut self) -> bool {
        if let Some(path) = self.current_project_path.clone() {
            self.save_project_to_path(path)
        } else {
            self.save_session_from_dialog()
        }
    }

    pub(super) fn save_project_for_close(&mut self) -> bool {
        self.save_current_project()
    }

    pub(super) fn save_session_from_dialog(&mut self) -> bool {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save Mirante4D project package")
            .add_filter("Mirante4D project", &["m4dproj"])
            .set_file_name("project.m4dproj")
            .save_file()
        else {
            return false;
        };
        self.save_project_to_path(path)
    }

    pub(super) fn save_project_to_path(&mut self, path: PathBuf) -> bool {
        match write_session_file_for_state(&path, &mut self.state) {
            Ok(()) => {
                self.state.last_workflow_message =
                    Some(format!("Saved project {}", path.display()));
                self.state.last_render_error = None;
                self.current_project_path = Some(path);
                self.mark_project_clean();
                true
            }
            Err(err) => {
                self.state.last_render_error = Some(err.to_string());
                tracing::error!(error = %err, "failed to save session");
                false
            }
        }
    }
}
