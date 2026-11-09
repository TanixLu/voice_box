mod batch_rename;
mod dialogs;
mod fonts;
mod hotkey_binding;
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

use egui_winit::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use strum::IntoEnumIterator;
use voice_box_shared::apo_cfg::ApoCfg;
use windows::Win32::Foundation::HWND;

use crate::{
    gui::{
        microphone::MicrophonePanel, soundboard_panel::SoundboardPanel, style::apply_style,
        voice::VoicePanel, widgets::section,
    },
    i18n::{self, Details, LanguagePreference, Message, Text, UserContext},
    paths::ui_state_path,
    settings::Settings,
    soundboard::library::Library,
    static_data::ICON_PNG_DATA,
    task::{self, Task, Wake},
};

/// What a part of the app is doing that the other parts must wait for.
#[derive(Clone, Copy, Default)]
pub struct Busy {
    /// Background work that must finish before other edits, exit or uninstall.
    pub working: bool,
    /// Audio devices are in use for playback, recording or preview.
    pub audio: bool,
}
impl Busy {
    pub fn any(self) -> bool {
        self.working || self.audio
    }
}
impl std::ops::BitOr for Busy {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self {
            working: self.working || other.working,
            audio: self.audio || other.audio,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Microphones,
    Voice,
    Soundboard,
}

/// The root window, which eframe keeps for the whole run, for Win32 features that deliver messages to it.
fn main_window(cc: &eframe::CreationContext<'_>) -> anyhow::Result<HWND> {
    let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
        anyhow::bail!("Expected a Win32 window");
    };
    Ok(HWND(handle.hwnd.get() as _))
}

fn close_to_tray(ctx: &egui::Context, exiting: bool, tray_available: bool) {
    if !exiting && ctx.input(|input| input.viewport().close_requested()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if tray_available {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }
}

pub fn run(settings: Settings, start_in_tray: bool) -> anyhow::Result<()> {
    let apo_config = ApoCfg::load().unwrap_or_else(|error| {
        tracing::warn!("Failed to load the APO configuration; using defaults: {error:#}");
        ApoCfg::default()
    });
    let library = Library::try_load()?;
    let icon = eframe::icon_data::from_png_bytes(ICON_PNG_DATA)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_visible(!start_in_tray)
            .with_icon(icon)
            .with_inner_size([560.0, 760.0])
            .with_min_inner_size([380.0, 340.0]),
        persist_window: true,
        persistence_path: Some(ui_state_path().to_path_buf()),
        ..Default::default()
    };
    eframe::run_native(
        "VoiceBox",
        options,
        Box::new(move |cc| {
            Ok(Box::new(VoiceBoxApp::new(
                cc,
                apo_config,
                settings,
                library,
                start_in_tray,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Voice,
    Soundboard,
}

/// The logon task's state; a notice is shown once a sync finishes.
enum Startup {
    Pending,
    Syncing(Task<bool>, Option<Text>),
    Synced,
    Unsynced,
}

enum Lifecycle {
    Running,
    ConfirmUninstall,
    Uninstalling(Task<()>),
    Exiting,
}

struct VoiceBoxApp {
    wake: Wake,
    tab: Tab,
    settings: Settings,
    settings_save: Option<Task<Settings>>,
    startup: Startup,
    lifecycle: Lifecycle,
    microphones: MicrophonePanel,
    voice: VoicePanel,
    soundboard: SoundboardPanel,
    /// Launched hidden and not yet shown; cleared on the first tray "Show".
    first_show_pending: bool,
    tray: tray::Tray,
    show_licenses: bool,
    notice: Option<(Text, Message)>,
}

impl VoiceBoxApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        apo_config: ApoCfg,
        settings: Settings,
        library: Library,
        start_in_tray: bool,
    ) -> anyhow::Result<Self> {
        let ctx = &cc.egui_ctx;
        let window = main_window(cc).user_context(Text::MainWindowUnavailable)?;
        let tray = tray::Tray::new(ctx, window, start_in_tray).user_context(Text::CreateTray)?;
        ctx.set_theme(settings.theme_preference);
        apply_style(ctx);
        fonts::install(ctx);
        let wake: Wake = {
            let ctx = ctx.clone();
            std::sync::Arc::new(move || ctx.request_repaint())
        };
        let mut app = Self {
            tab: Tab::Voice,
            soundboard: SoundboardPanel::new(
                wake.clone(),
                window,
                library,
                settings.soundboard.clone(),
            ),
            settings,
            settings_save: None,
            startup: Startup::Pending,
            lifecycle: Lifecycle::Running,
            microphones: MicrophonePanel::new(wake.clone()),
            voice: VoicePanel::new(apo_config, wake.clone()),
            wake,
            first_show_pending: start_in_tray,
            tray,
            show_licenses: false,
            notice: None,
        };
        app.microphones.refresh();
        Ok(app)
    }

    /// What the app and every panel except `except` are doing.
    fn busy(&self, except: Option<Part>) -> Busy {
        let own = Busy {
            working: self.settings_save.is_some()
                || matches!(self.startup, Startup::Syncing(..))
                || matches!(
                    self.lifecycle,
                    Lifecycle::Uninstalling(_) | Lifecycle::Exiting
                ),
            audio: false,
        };
        [
            (Part::Microphones, self.microphones.busy()),
            (Part::Voice, self.voice.busy()),
            (Part::Soundboard, self.soundboard.busy()),
        ]
        .into_iter()
        .filter(|(part, _)| Some(*part) != except)
        .fold(own, |all, (_, busy)| all | busy)
    }

    /// Background work is running, or the app is going away.
    fn interaction_blocked(&self) -> bool {
        self.busy(None).working
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if !self.interaction_blocked() {
            self.exit(ctx);
        }
    }

    fn exit(&mut self, ctx: &egui::Context) {
        self.lifecycle = Lifecycle::Exiting;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn save_settings(&mut self, settings: Settings) {
        if self.settings_save.is_none() {
            let task = Task::run("VoiceBoxSettingsSave", &self.wake, move || {
                settings.save()?;
                Ok(settings)
            });
            self.settings_save = Some(task);
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context, settings: Settings) {
        let startup_changed = settings.startup_enabled != self.settings.startup_enabled;
        ctx.set_theme(settings.theme_preference);
        i18n::set_language(settings.language_preference.resolve());
        self.soundboard.apply_prefs(settings.soundboard.clone());
        self.settings = settings;
        if startup_changed {
            let notice = self
                .settings
                .startup_enabled
                .then_some(Text::StartupRegistered);
            self.synchronize_startup(notice);
        }
    }

    fn synchronize_startup(&mut self, notice: Option<Text>) {
        let enabled = self.settings.startup_enabled;
        let task = Task::run("VoiceBoxStartupSync", &self.wake, move || {
            crate::startup::synchronize_startup(enabled)
        });
        self.startup = Startup::Syncing(task, notice);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        match task::finished(&mut self.settings_save) {
            Some(Ok(settings)) => self.apply_settings(ctx, settings),
            Some(Err(error)) => self.notice = Some((Text::Settings, Message::error(error))),
            None => {}
        }
        if let Startup::Syncing(task, notice) = &mut self.startup
            && let Some(result) = task.poll()
        {
            let notice = *notice;
            self.startup = if result.is_ok() {
                Startup::Synced
            } else {
                Startup::Unsynced
            };
            let message = match result {
                Ok(changed) if changed || notice.is_some() => {
                    Some(notice.unwrap_or(Text::StartupReRegistered).into())
                }
                Ok(_) => None,
                Err(error) => Some(Message::error(error)),
            };
            if let Some(message) = message {
                self.notice = Some((Text::Startup, message));
                if !self.first_show_pending {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                }
            }
        }
        if let Lifecycle::Uninstalling(task) = &mut self.lifecycle
            && let Some(result) = task.poll()
        {
            match result {
                Ok(()) => self.exit(ctx),
                Err(error) => {
                    self.lifecycle = Lifecycle::Running;
                    let error = Message::UninstallFailed {
                        error: Details::new(error),
                    };
                    self.notice = Some((Text::UninstallVoiceBox, error));
                }
            }
        }
        self.microphones.poll();
        self.voice.poll();
        self.soundboard.poll(ctx);
    }

    fn startup_ui(&mut self, ui: &mut egui::Ui) {
        let previous = self.settings.startup_enabled;
        let mut enabled = previous;
        ui.label(Text::StartupLabel.text());
        let pending = matches!(self.startup, Startup::Pending);
        let synced = matches!(self.startup, Startup::Synced);
        let retry = ui
            .add_enabled_ui(!pending && !self.interaction_blocked(), |ui| {
                ui.selectable_value(&mut enabled, true, Text::StartupOn.text());
                ui.selectable_value(&mut enabled, false, Text::StartupOff.text());
                if !synced {
                    ui.label(Text::StartupNotSynced.text());
                    return ui.button(Text::RetryStartup.text()).clicked();
                }
                false
            })
            .inner;
        if previous != enabled {
            let settings = Settings {
                startup_enabled: enabled,
                ..self.settings.clone()
            };
            self.save_settings(settings);
            ui.close();
        } else if retry {
            self.synchronize_startup(enabled.then_some(Text::StartupRegistered));
            ui.close();
        }
    }

    fn theme_ui(&mut self, ui: &mut egui::Ui) {
        use egui::ThemePreference;
        let mut theme_preference = self.settings.theme_preference;
        ui.label(Text::ThemeLabel.text());
        for (preference, label) in [
            (ThemePreference::System, Text::FollowSystem),
            (ThemePreference::Light, Text::LightTheme),
            (ThemePreference::Dark, Text::DarkTheme),
        ] {
            ui.selectable_value(&mut theme_preference, preference, label.text());
        }
        if theme_preference != self.settings.theme_preference {
            let settings = Settings {
                theme_preference,
                ..self.settings.clone()
            };
            self.save_settings(settings);
        }
    }

    fn language_ui(&mut self, ui: &mut egui::Ui) {
        let mut language_preference = self.settings.language_preference;
        ui.label(Text::LanguageLabel.text());
        for preference in LanguagePreference::iter() {
            ui.selectable_value(&mut language_preference, preference, preference.text());
        }
        if language_preference != self.settings.language_preference {
            let settings = Settings {
                language_preference,
                ..self.settings.clone()
            };
            self.save_settings(settings);
        }
    }

    fn more_menu_ui(&mut self, ui: &mut egui::Ui) {
        let more = ui.button(Text::More.text());
        egui::Popup::menu(&more)
            .align(egui::RectAlign::BOTTOM)
            .show(|ui| {
                ui.set_min_width(100.0);
                ui.spacing_mut().button_padding.x = 25.0;

                ui.add_enabled_ui(!self.interaction_blocked(), |ui| self.language_ui(ui));
                ui.separator();

                ui.add_enabled_ui(!self.interaction_blocked(), |ui| self.theme_ui(ui));
                ui.separator();

                self.startup_ui(ui);
                ui.separator();

                if ui
                    .add_enabled(
                        !self.busy(None).any(),
                        egui::Button::new(Text::FullUninstall.text()),
                    )
                    .clicked()
                {
                    self.microphones.cancel_uninstall();
                    self.lifecycle = Lifecycle::ConfirmUninstall;
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
    }

    fn voice_tab_ui(&mut self, ui: &mut egui::Ui) {
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
                            let microphone_others = self.busy(Some(Part::Microphones));
                            let voice_others = self.busy(Some(Part::Voice));
                            section(ui, |ui| {
                                self.microphones.ui(ui, microphone_others);
                            });
                            section(ui, |ui| self.voice.config_ui(ui));
                            section(ui, |ui| {
                                let microphone = self.microphones.selected();
                                self.voice.recording_ui(ui, voice_others, microphone)
                            });
                            self.microphones.status_ui(ui);
                        },
                    );
                });
            });
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let soundboard_visible = self.tab == Tab::Soundboard;
        if !self.interaction_blocked() {
            self.soundboard.prepare(ui.ctx(), soundboard_visible);
        }
        if matches!(self.lifecycle, Lifecycle::Uninstalling(_)) {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(64.0);
                    ui.heading(egui::RichText::new(Text::UninstallingVoiceBox.text()).size(25.0));
                    ui.add_space(20.0);
                    ui.spinner();
                    ui.add_space(12.0);
                    ui.label(Text::PreparingUninstall.text());
                });
            });
            return;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(20))
            .show(ui, |ui| {
                if self.interaction_blocked() || self.soundboard.capturing() {
                    ui.disable();
                }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Voice, Text::Voice.text());
                    ui.selectable_value(&mut self.tab, Tab::Soundboard, Text::Soundboard.text());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.more_menu_ui(ui);
                    });
                });

                ui.add_space(12.0);

                if self.interaction_blocked() && ui.is_enabled() {
                    ui.disable();
                }
                match self.tab {
                    Tab::Voice => self.voice_tab_ui(ui),
                    Tab::Soundboard => {
                        let others = self.busy(Some(Part::Soundboard));
                        self.soundboard.ui(ui, others)
                    }
                }
            });

        let ctx = ui.ctx();
        self.licenses_ui(ctx);
        let blocked = self.interaction_blocked();
        if let Some(prefs) = self.soundboard.windows(ctx, blocked, soundboard_visible) {
            let settings = Settings {
                soundboard: prefs,
                ..self.settings.clone()
            };
            self.save_settings(settings);
        }
        self.full_uninstall_ui(ctx);
        self.notice_ui(ctx);
    }
}

impl eframe::App for VoiceBoxApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if matches!(self.startup, Startup::Pending) && !self.interaction_blocked() {
            self.synchronize_startup(None);
        }
        while let Ok(action) = self.tray.receiver.try_recv() {
            match action {
                tray::Action::Show => {
                    // eframe may already consider its suppressed first show visible.
                    if std::mem::take(&mut self.first_show_pending) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                tray::Action::Exit => self.request_exit(ctx),
            }
        }
        let exiting = matches!(self.lifecycle, Lifecycle::Exiting);
        close_to_tray(ctx, exiting, self.tray.available());
        self.tray.set_exit_enabled(!self.interaction_blocked());
        let shortcuts_enabled = !self.busy(Some(Part::Soundboard)).any()
            && !matches!(self.lifecycle, Lifecycle::ConfirmUninstall);
        self.soundboard.shortcuts(ctx, shortcuts_enabled);
        // Keep completions moving even when eframe skips painting a hidden window.
        if self.busy(None).any() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
        // Include operations started during this UI pass before the native menu can open.
        self.tray.set_exit_enabled(!self.interaction_blocked());
        let allowed = matches!(
            self.lifecycle,
            Lifecycle::Running | Lifecycle::ConfirmUninstall
        );
        self.soundboard.overlay_ui(ui.ctx(), allowed);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.soundboard.shutdown();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
}
