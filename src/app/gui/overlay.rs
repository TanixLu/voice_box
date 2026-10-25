use super::soundboard_panel::format_duration;
use crate::app::i18n::Text;
use crate::app::i18n::UserContext;
use crate::app::soundboard::Soundboard;
use anyhow::ensure;
use egui::{Color32, FontId, Rect, Vec2, pos2, vec2};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        UI::{
            HiDpi::GetDpiForSystem,
            Shell::{DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                FindWindowW, GetWindowThreadProcessId, SPI_GETWORKAREA, SWP_FRAMECHANGED,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowPos, SystemParametersInfoW,
                WM_NCCALCSIZE, WM_NCDESTROY,
            },
        },
    },
    core::PCWSTR,
};

const SIZE: Vec2 = vec2(360.0, 72.0);
// This undecorated tool window uses an invariant internal title for native lookup.
const NATIVE_TITLE: &str = "VoiceBox Playback Overlay";

#[derive(Default)]
pub(super) struct Overlay {
    builder: Option<egui::ViewportBuilder>,
}

impl Overlay {
    pub(super) fn show(
        &mut self,
        ctx: &egui::Context,
        model: &Soundboard,
        enabled: bool,
    ) -> anyhow::Result<bool> {
        let sound = model
            .active_sound_index
            .filter(|_| enabled && !model.importing())
            .and_then(|index| model.library().sounds.get(index));
        let mut layout_result = Ok(());
        let playback = sound.and_then(|sound| match viewport_builder(ctx) {
            Ok(builder) => {
                self.builder = Some(builder);
                Some((
                    sound.name.clone(),
                    model.elapsed,
                    model.duration.or(Some(sound.duration)),
                    model.options().repeat,
                ))
            }
            Err(error) => {
                layout_result = Err(error);
                None
            }
        });
        let visible = playback.is_some();
        if let Some(builder) = &self.builder {
            let id = egui::ViewportId::from_hash_of("sound_playback_overlay");
            // Keep declaring the viewport after first use, even when hidden or layout fails.
            // Omitting it would destroy the native window and its GL surface.
            ctx.show_viewport_deferred(id, builder.clone().with_visible(visible), move |ui, _| {
                if let Some((name, elapsed, duration, repeat)) = &playback {
                    draw(ui, name, *elapsed, *duration, *repeat);
                }
            });
            if visible {
                ctx.request_repaint_of(id);
            }
        }
        layout_result?;
        Ok(visible)
    }
}

fn viewport_builder(ctx: &egui::Context) -> anyhow::Result<egui::ViewportBuilder> {
    let mut work = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .user_context(Text::ReadWorkArea)?;
    }
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0 * ctx.zoom_factor();
    ensure!(
        scale > 0.0 && work.right > work.left && work.bottom > work.top,
        Text::InvalidDisplay
    );
    let work = Rect::from_min_max(
        pos2(work.left as f32, work.top as f32) / scale,
        pos2(work.right as f32, work.bottom as f32) / scale,
    );
    let size = SIZE.min(work.size());
    let position = pos2(
        work.center().x - size.x / 2.0,
        (work.bottom() - size.y - 24.0).max(work.top()),
    );
    Ok(egui::ViewportBuilder::default()
        .with_title(NATIVE_TITLE)
        .with_inner_size(size)
        .with_position(position)
        .with_transparent(true)
        .with_decorations(false)
        .with_resizable(false)
        .with_active(false)
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_always_on_top())
}

fn draw(ui: &mut egui::Ui, name: &str, elapsed: f64, duration: Option<f64>, repeat: bool) {
    if let Err(error) = remove_native_frame() {
        tracing::warn!("Failed to remove overlay window frame: {error:#}");
    }
    let rect = ui.max_rect();
    let painter = ui.painter();
    painter.rect_filled(rect, 12, Color32::from_rgba_unmultiplied(24, 26, 32, 230));
    let duration = duration.filter(|d| d.is_finite() && *d > 0.0);
    let elapsed = match duration {
        Some(duration) if repeat => elapsed.max(0.0) % duration,
        Some(duration) => elapsed.max(0.0).min(duration),
        None => elapsed.max(0.0),
    };
    let time = match duration {
        Some(duration) => format!(
            "{} / {}",
            format_duration(elapsed),
            format_duration(duration)
        ),
        None => format_duration(elapsed),
    };
    painter.text(
        rect.min + vec2(12.0, 32.0),
        egui::Align2::LEFT_TOP,
        time,
        FontId::proportional(12.0),
        Color32::from_gray(190),
    );
    if let Some(duration) = duration {
        let bar = Rect::from_min_size(
            pos2(rect.left() + 12.0, rect.bottom() - 10.0),
            vec2((rect.width() - 24.0).max(0.0), 3.0),
        );
        painter.rect_filled(bar, 2, Color32::from_white_alpha(35));
        let filled = Rect::from_min_size(
            bar.min,
            vec2(bar.width() * (elapsed / duration) as f32, bar.height()),
        );
        painter.rect_filled(filled, 2, Color32::from_rgb(114, 180, 255));
    }
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(Rect::from_min_size(
            rect.min + vec2(12.0, 8.0),
            vec2((rect.width() - 24.0).max(0.0), 20.0),
        )),
        |ui| {
            ui.horizontal(|ui| {
                if repeat {
                    ui.label(
                        egui::RichText::new(Text::Repeat.text())
                            .size(12.0)
                            .color(Color32::from_rgb(114, 180, 255)),
                    );
                }
                ui.add(
                    egui::Label::new(egui::RichText::new(name).size(15.0).color(Color32::WHITE))
                        .truncate(),
                );
            });
        },
    );
}

fn remove_native_frame() -> anyhow::Result<()> {
    unsafe {
        let title = crate::shared::utils::wide_null(NATIVE_TITLE);
        let window = FindWindowW(None, PCWSTR(title.as_ptr()))?;
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        ensure!(process_id == std::process::id(), Text::WrongOverlayProcess);
        let mut data = 0;
        if !GetWindowSubclass(window, Some(overlay_window_proc), 0, Some(&mut data)).as_bool() {
            SetWindowSubclass(window, Some(overlay_window_proc), 0, 0)
                .ok()
                .user_context(Text::InstallOverlayCallback)?;
            SetWindowPos(
                window,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            )?;
        }
    }
    Ok(())
}

unsafe extern "system" fn overlay_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    _data: usize,
) -> LRESULT {
    // Bypass winit's shadow workaround, which reserves a 1px non-client strip at the top.
    if message == WM_NCCALCSIZE {
        return LRESULT(0);
    }
    unsafe {
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(window, Some(overlay_window_proc), subclass_id);
        }
        DefSubclassProc(window, message, wparam, lparam)
    }
}
