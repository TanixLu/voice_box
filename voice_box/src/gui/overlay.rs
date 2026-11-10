use anyhow::ensure;
use egui::{Color32, FontId, Rect, Vec2, pos2, vec2};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        UI::{
            HiDpi::GetDpiForSystem,
            Shell::{DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                FindWindowW, GW_HWNDPREV, GetWindow, GetWindowThreadProcessId, HWND_TOPMOST,
                SPI_GETWORKAREA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
                SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowPos,
                SystemParametersInfoW, WM_NCCALCSIZE, WM_NCDESTROY,
            },
        },
    },
    core::PCWSTR,
};

use crate::{
    gui::widgets::format_duration,
    i18n::{Text, UserContext},
    soundboard::NowPlaying,
};

const SIZE: Vec2 = vec2(360.0, 72.0);
// This undecorated tool window uses an invariant internal title for native lookup.
const NATIVE_TITLE: &str = "VoiceBox Playback Overlay";

#[derive(Default)]
pub struct Overlay {
    builder: Option<egui::ViewportBuilder>,
}

impl Overlay {
    /// Shows `playing`, or hides the overlay when it is `None`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        playing: Option<NowPlaying>,
    ) -> anyhow::Result<bool> {
        let mut layout_result = Ok(());
        let playback = playing.and_then(|playing| match viewport_builder(ctx) {
            Ok(builder) => {
                self.builder = Some(builder);
                Some(playing)
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
                if let Some(playing) = &playback {
                    draw(ui, playing);
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
        .user_context(Text::ReadWorkAreaFailed)?;
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

fn draw(ui: &mut egui::Ui, playing: &NowPlaying) {
    let (name, duration_secs, repeat) = (&playing.name, playing.duration_secs, playing.repeat);
    if let Err(error) = configure_native_window() {
        tracing::warn!("Failed to configure overlay window: {error:#}");
    }
    let rect = ui.max_rect();
    let painter = ui.painter();
    painter.rect_filled(rect, 12, Color32::from_rgba_unmultiplied(24, 26, 32, 230));
    let elapsed_secs = playing.elapsed_secs;
    let elapsed_secs = match duration_secs {
        Some(duration_secs) if repeat => elapsed_secs.max(0.0) % duration_secs,
        Some(duration_secs) => elapsed_secs.max(0.0).min(duration_secs),
        None => elapsed_secs.max(0.0),
    };
    let time = match duration_secs {
        Some(duration_secs) => format!(
            "{} / {}",
            format_duration(elapsed_secs),
            format_duration(duration_secs)
        ),
        None => format_duration(elapsed_secs),
    };
    painter.text(
        rect.min + vec2(12.0, 32.0),
        egui::Align2::LEFT_TOP,
        time,
        FontId::proportional(12.0),
        Color32::from_gray(190),
    );
    if let Some(duration_secs) = duration_secs {
        let bar = Rect::from_min_size(
            pos2(rect.left() + 12.0, rect.bottom() - 10.0),
            vec2((rect.width() - 24.0).max(0.0), 3.0),
        );
        painter.rect_filled(bar, 2, Color32::from_white_alpha(35));
        let filled = Rect::from_min_size(
            bar.min,
            vec2(
                bar.width() * (elapsed_secs / duration_secs) as f32,
                bar.height(),
            ),
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

fn configure_native_window() -> anyhow::Result<()> {
    unsafe {
        let title = voice_box_shared::win::wide_null(NATIVE_TITLE);
        let window = FindWindowW(None, PCWSTR(title.as_ptr()))?;
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        ensure!(process_id == std::process::id(), Text::WrongOverlayProcess);
        let mut data = 0;
        if !GetWindowSubclass(window, Some(overlay_window_proc), 0, Some(&mut data)).as_bool() {
            SetWindowSubclass(window, Some(overlay_window_proc), 0, 0)
                .ok()
                .user_context(Text::InstallOverlayCallbackFailed)?;
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
        // Topmost games re-raise themselves above us; reclaim the top of the z-order.
        if GetWindow(window, GW_HWNDPREV).is_ok() {
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER,
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
