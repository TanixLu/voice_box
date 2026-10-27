mod batch_rename;
mod dialogs;
mod fonts;
mod hotkey_binding;
mod jobs;
mod microphone;
mod overlay;
mod picker;
mod recording_import;
mod sound_list;
mod sound_settings;
mod soundboard_panel;
mod style;
mod tray;
mod voice;
mod widgets;

use crate::i18n::Text;
use crate::i18n::{Details, Message, UserContext};
use crate::{apo_cfg_writer, settings, soundboard::library::Library};
use hotkey_binding::{HotkeyManager, PlaybackAction};
use jobs::{JobResult, Jobs};
use microphone::MicrophonePanel;
use soundboard_panel::SoundboardPanel;
use style::apply_style;
use voice::VoicePanel;
use voice_box_shared::{
    apo_cfg::{ApoCfgFile, ApoConfig},
    paths,
};
use widgets::section;

fn wake_ui(ctx: egui::Context) -> impl Fn() + Clone + Send + 'static {
    move || ctx.request_repaint()
}

fn close_to_tray(ctx: &egui::Context, exiting: bool, tray_available: bool) {
    if !exiting && ctx.input(|input| input.viewport().close_requested()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if tray_available {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }
}

pub fn run(settings: settings::Settings) -> anyhow::Result<()> {
    let apo_config = ApoCfgFile::try_load()?.config;
    let library = Library::try_load()?;
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("OUT_DIR"), "/icon.png")))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_inner_size([560.0, 760.0])
            .with_min_inner_size([380.0, 340.0]),
        // Track normal geometry every frame instead of only at eframe's save interval.
        // eframe still restores the saved "window" entry on startup.
        persist_window: false,
        persistence_path: Some(paths::data_dir()?.join(super::UI_STATE_FILE)),
        ..Default::default()
    };
    eframe::run_native(
        "VoiceBox",
        options,
        Box::new(move |cc| {
            Ok(Box::new(VoiceBoxApp::new(
                cc, apo_config, settings, library,
            )?))
        }),
    )
    .map_err(|error| {
        anyhow::anyhow!(Message::OpenWindowFailed {
            error: Details::new(anyhow::anyhow!(error.to_string()))
        })
    })?;
    // The cleanup process waits for this entire process to exit, including all
    // eframe persistence writes, before recycling the data directory.
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct UiPreferences {
    selected_microphone_guid: Option<String>,
    zoom_factor: f32,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            selected_microphone_guid: None,
            zoom_factor: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Voice,
    Soundboard,
}

struct VoiceBoxApp {
    window_settings: Option<egui_winit::WindowSettings>,
    tab: Tab,
    zoom_factor: f32,
    microphones: MicrophonePanel,
    voice: VoicePanel,
    soundboard: SoundboardPanel,
    hotkeys: HotkeyManager,
    jobs: Jobs,
    tray: tray::Tray,
    exit_requested: bool,
    confirm_full_uninstall: bool,
    uninstalling: bool,
    uninstall_requested: bool,
    show_licenses: bool,
    overlay: overlay::Overlay,
    overlay_visible: bool,
}

impl VoiceBoxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        apo_config: ApoConfig,
        settings: settings::Settings,
        library: Library,
    ) -> anyhow::Result<Self> {
        let ctx = &cc.egui_ctx;
        let tray = tray::Tray::new(cc).user_context(Text::CreateTray)?;
        let preferences: UiPreferences = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, "voice_box.ui"))
            .unwrap_or_default();
        if preferences.zoom_factor.is_finite() && preferences.zoom_factor > 0.0 {
            ctx.set_zoom_factor(preferences.zoom_factor);
        }
        ctx.set_theme(settings.theme);
        apply_style(ctx);
        fonts::install(ctx);
        let mut app = Self {
            window_settings: cc
                .storage
                .and_then(|storage| eframe::get_value(storage, "window")),
            tab: Tab::Voice,
            zoom_factor: ctx.zoom_factor(),
            microphones: MicrophonePanel::new(preferences.selected_microphone_guid),
            voice: VoicePanel::new(apo_config),
            soundboard: SoundboardPanel::new(settings, library),
            hotkeys: HotkeyManager::new(ctx),
            jobs: Jobs::default(),
            tray,
            exit_requested: false,
            confirm_full_uninstall: false,
            uninstalling: false,
            uninstall_requested: false,
            show_licenses: false,
            overlay: overlay::Overlay::default(),
            overlay_visible: false,
        };
        app.microphones.refresh(ctx, &mut app.jobs);
        Ok(app)
    }

    fn interaction_blocked(&self) -> bool {
        self.soundboard.interaction_blocked(&self.jobs)
            || self.uninstalling
            || self.uninstall_requested
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if !self.interaction_blocked() {
            self.exit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        self.zoom_factor = ui.ctx().zoom_factor();
        if self.tab != Tab::Soundboard {
            self.soundboard.cancel_batch();
            if matches!(
                self.hotkeys.target(),
                Some(crate::settings::BindingTarget::Sound(_))
            ) {
                self.hotkeys.cancel_capture();
            }
        }
        if !self.interaction_blocked() {
            self.soundboard.poll_capture(ui.ctx(), &mut self.hotkeys);
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                self.soundboard.cancel_rename();
            }
            if self.tab != Tab::Soundboard
                || ui.input(|input| {
                    !input.focused
                        || input.smooth_scroll_delta != egui::Vec2::ZERO
                        || input.pointer.is_decidedly_dragging()
                        || input.key_pressed(egui::Key::Escape)
                })
            {
                self.soundboard.cancel_delete();
            }
        }
        if self.uninstalling {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(64.0);
                    ui.heading(egui::RichText::new(Text::UninstallingVoiceBox.text()).size(25.0));
                    ui.add_space(20.0);
                    ui.spinner();
                    ui.add_space(12.0);
                    ui.label(self.jobs.status.text());
                });
            });
            return;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(20))
            .show(ui, |ui| {
                if self.interaction_blocked() {
                    ui.disable();
                }
                ui.horizontal(|ui| {
                    if self.hotkeys.target().is_some() {
                        ui.disable();
                    }
                    ui.selectable_value(&mut self.tab, Tab::Voice, Text::Voice.text());
                    ui.selectable_value(&mut self.tab, Tab::Soundboard, Text::Soundboard.text());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let more = ui.button(Text::More.text());
                        egui::Popup::menu(&more)
                            .align(egui::RectAlign::BOTTOM)
                            .show(|ui| {
                                ui.set_min_width(100.0);
                                ui.spacing_mut().button_padding.x = 25.0;
                                self.language_ui(ui);
                                ui.separator();
                                self.theme_ui(ui);
                                ui.separator();
                                if ui
                                    .add_enabled(
                                        !self.interaction_blocked()
                                            && !self.voice.session.busy()
                                            && !self.soundboard.model.playing(),
                                        egui::Button::new(Text::FullUninstall.text()),
                                    )
                                    .clicked()
                                {
                                    self.microphones.cancel_uninstall();
                                    self.confirm_full_uninstall = true;
                                    ui.close();
                                }
                                if ui.button(Text::Licenses.text()).clicked() {
                                    self.show_licenses = true;
                                    ui.close();
                                }
                                ui.separator();
                                if ui
                                    .add_enabled(
                                        !self.interaction_blocked(),
                                        egui::Button::new(Text::Exit.text()),
                                    )
                                    .clicked()
                                {
                                    self.request_exit(ui.ctx());
                                    ui.close();
                                }
                            });
                    });
                });
                ui.add_space(12.0);
                if self.tab == Tab::Soundboard {
                    self.soundboard.ui(
                        ui,
                        &mut self.jobs,
                        self.voice.session.busy(),
                        &mut self.hotkeys,
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("voice_tab_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                let width = ui.available_width().min(680.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width, 0.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        section(ui, |ui| {
                                            self.microphones.ui(
                                                ui,
                                                self.voice.session.busy()
                                                    || self.soundboard.model.busy(),
                                                &mut self.jobs,
                                            );
                                        });
                                        section(ui, |ui| {
                                            if let Some(config) =
                                                self.voice.config_ui(ui, self.jobs.busy())
                                            {
                                                self.jobs.begin(
                                                    ui.ctx(),
                                                    Text::SavingApoCfg,
                                                    move || JobResult::Saved {
                                                        result: apo_cfg_writer::save(
                                                            config.clone(),
                                                        ),
                                                        config,
                                                    },
                                                );
                                            }
                                        });
                                        section(ui, |ui| {
                                            self.voice.recording_ui(
                                                ui,
                                                self.jobs.busy() || self.soundboard.model.busy(),
                                                self.microphones.selected(),
                                            )
                                        });
                                        self.microphones.status_ui(ui, &self.jobs);
                                    },
                                );
                            });
                        });
                }
            });

        self.licenses_ui(ui.ctx());
        self.soundboard
            .settings_ui(ui.ctx(), &mut self.jobs, &mut self.hotkeys);
        self.soundboard.recording_ui(ui.ctx());
        if !self.interaction_blocked() && !self.soundboard.recording_open() {
            self.soundboard.finish_rename(self.tab == Tab::Soundboard);
        }
        self.soundboard.file_error_ui(ui.ctx());
        self.full_uninstall_ui(ui.ctx());
    }
}

impl eframe::App for VoiceBoxApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(window) = frame.winit_window()
            && window.is_minimized() != Some(true)
        {
            self.window_settings = Some(egui_winit::WindowSettings::from_window(
                ctx.zoom_factor(),
                window,
                self.window_settings
                    .map(|settings| settings.normal_geometry()),
            ));
        }
        self.poll_job();
        self.voice.session.poll();
        self.soundboard.model.poll(wake_ui(ctx.clone()));
        self.soundboard.finish_delete();
        self.soundboard.poll_recording(ctx);
        while let Ok(action) = self.tray.receiver.try_recv() {
            match action {
                tray::Action::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                tray::Action::Exit => self.request_exit(ctx),
            }
        }
        close_to_tray(
            ctx,
            self.exit_requested || self.uninstall_requested,
            self.tray.available(),
        );
        self.tray.set_exit_enabled(!self.interaction_blocked());
        let focused = ctx.input(|input| {
            input.viewport().focused == Some(true) && input.viewport().minimized != Some(true)
        });
        if !focused {
            self.hotkeys.cancel_capture();
        }
        let shortcuts_enabled = !self.interaction_blocked()
            && !self.voice.session.busy()
            && self.soundboard.model.file_error.is_none()
            && !self.soundboard.recording_open()
            && !self.confirm_full_uninstall
            && !(focused && (self.soundboard.renaming() || ctx.text_edit_focused()));
        let bindings = std::iter::once(self.soundboard.model.settings.playback_stop_hotkey)
            .chain(std::iter::once(
                self.soundboard.model.settings.playback_random_hotkey,
            ))
            .chain(
                self.soundboard
                    .model
                    .library()
                    .sounds
                    .iter()
                    .map(|sound| sound.hotkey),
            );
        for action in self.hotkeys.poll(shortcuts_enabled, bindings) {
            match action {
                PlaybackAction::Stop => self.soundboard.model.stop(),
                PlaybackAction::Random(hotkey) => {
                    self.soundboard
                        .play_random_sound(ctx, hotkey, &mut self.hotkeys)
                }
            }
        }
        if self.uninstall_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Bootstrap the child while minimized; visible children keep UI passes running.
        if self.soundboard.model.settings.playback_show_overlay
            && self.soundboard.model.playing()
            && !self.overlay_visible
            && ctx.input(|input| input.viewport().visible() == Some(false))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
        // Keep completions moving even when eframe skips painting a hidden window.
        if self.jobs.busy()
            || self.voice.session.busy()
            || self.soundboard.model.busy()
            || self.soundboard.recording_open()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.soundboard.model.stop();
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(settings) = &self.window_settings {
            eframe::set_value(storage, "window", settings);
        }
        eframe::set_value(
            storage,
            "voice_box.ui",
            &UiPreferences {
                selected_microphone_guid: self.microphones.preferred_guid(),
                zoom_factor: self.zoom_factor,
            },
        );
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
        // Include operations started during this UI pass before the native menu can open.
        self.tray.set_exit_enabled(!self.interaction_blocked());
        self.overlay_visible = false;
        let overlay_enabled = self.soundboard.model.settings.playback_show_overlay
            && !self.uninstalling
            && !self.uninstall_requested;
        match self
            .overlay
            .show(ui.ctx(), &self.soundboard.model, overlay_enabled)
        {
            Ok(visible) => self.overlay_visible = visible,
            Err(error) => {
                tracing::warn!("Failed to show playback overlay: {error:#}");
                self.soundboard.model.set_error(Message::ShowOverlayFailed {
                    error: Details::new(error),
                });
            }
        }
    }
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
}

impl VoiceBoxApp {
    fn theme_ui(&mut self, ui: &mut egui::Ui) {
        use egui::ThemePreference;
        let previous = self.soundboard.model.settings.theme;
        let mut theme = previous;
        ui.label(Text::ThemeLabel.text());
        for (preference, label) in [
            (ThemePreference::System, Text::FollowSystem),
            (ThemePreference::Light, Text::LightTheme),
            (ThemePreference::Dark, Text::DarkTheme),
        ] {
            ui.selectable_value(&mut theme, preference, label.text());
        }
        if previous != theme {
            let mut settings = self.soundboard.model.settings.clone();
            settings.theme = theme;
            match settings.save() {
                Ok(()) => {
                    self.soundboard.model.settings = settings;
                    ui.ctx().set_theme(theme);
                    ui.ctx().request_repaint();
                }
                Err(error) => {
                    self.jobs.status = Message::error(error);
                    self.jobs.failed = true;
                }
            }
        }
    }

    fn language_ui(&mut self, ui: &mut egui::Ui) {
        use crate::i18n::{self, LanguagePreference};
        let previous = self.soundboard.model.settings.language;
        let mut language = previous;
        ui.label(Text::LanguageLabel.text());
        for preference in [
            LanguagePreference::System,
            LanguagePreference::English,
            LanguagePreference::SimplifiedChinese,
        ] {
            ui.selectable_value(&mut language, preference, preference.label());
        }
        if previous != language {
            let mut settings = self.soundboard.model.settings.clone();
            settings.language = language;
            match settings.save() {
                Ok(()) => {
                    self.soundboard.model.settings = settings;
                    i18n::set_language(language.resolve());
                    ui.ctx().request_repaint();
                }
                Err(error) => {
                    self.jobs.status = Message::error(error);
                    self.jobs.failed = true;
                }
            }
        }
    }
}
