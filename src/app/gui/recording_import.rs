use super::widgets::{ButtonIcon, hint, icon_button, warning};
use crate::app::{
    audio::loopback::MAX_RECORD_SECONDS,
    soundboard::{
        Soundboard,
        library::is_valid_file_name,
        recording::{Activity, Edge, Recorder, Selection},
    },
};

#[derive(Default)]
pub(super) struct RecordingImport {
    recorder: Recorder,
    selection: Option<Selection>,
    name: String,
    dragging: Option<Edge>,
}
impl RecordingImport {
    pub fn saving(&self) -> bool {
        self.recorder.activity() == Some(Activity::Saving)
    }

    // Called in app logic, including when the main viewport is hidden.
    pub fn poll(&mut self, ctx: &egui::Context, model: &mut Soundboard) -> bool {
        let had_clip = self.recorder.clip.is_some();
        if let Some(sound) = self.recorder.poll(super::wake_ui(ctx.clone())) {
            match model.add_recording(sound) {
                Ok(()) => return true,
                Err(error) => {
                    tracing::error!("Failed to commit recorded sound to library: {error:#}");
                    self.recorder.error = format!("导入失败：{error:#}");
                }
            }
        }
        if !had_clip && let Some(clip) = &self.recorder.clip {
            self.selection = Some(Selection::full(&clip.audio));
            if self.name.is_empty() {
                self.name = "录音".into();
            }
        }
        if self.recorder.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        false
    }

    // Ignore backdrop clicks so switching applications does not discard a clip.
    pub fn ui(&mut self, ctx: &egui::Context) -> bool {
        let mut close = false;
        let saving = self.saving();
        let available = ctx.content_rect().size();
        let content_height = (available.y - 110.0).clamp(100.0, 340.0);
        egui::Modal::new(egui::Id::unique("recording_import"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                ui.set_width((available.x - 64.0).clamp(240.0, 620.0));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("录制导入").size(17.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = icon_button(ui, !saving, ButtonIcon::Close).clicked();
                    });
                });
                ui.add_space(12.0);
                ui.spacing_mut().scroll.floating = false;
                egui::ScrollArea::vertical()
                    // Keep the complete editor visible instead of shrinking to its previous size.
                    .min_scrolled_height(content_height)
                    .max_height(content_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.record_ui(ui);
                        ui.add_space(8.0);
                        self.editor_ui(ui);
                        if !self.recorder.error.is_empty() {
                            ui.add_space(8.0);
                            warning(ui, &self.recorder.error);
                        }
                    });
            });
        !self.saving() && (close || ctx.input(|input| input.key_pressed(egui::Key::Escape)))
    }

    fn record_ui(&mut self, ui: &mut egui::Ui) {
        let recording = self.recorder.activity() == Some(Activity::Recording);
        let saving = self.saving();
        ui.horizontal_wrapped(|ui| {
            let label = if recording {
                "停止录制"
            } else {
                "开始录制"
            };
            if ui
                .add_enabled(
                    !saving && (!recording || !self.recorder.stopping()),
                    egui::Button::new(label),
                )
                .on_hover_text("录制默认扬声器；再次开始录制将替换当前录音")
                .clicked()
            {
                if recording {
                    self.recorder.stop();
                } else {
                    self.recorder.record(super::wake_ui(ui.ctx().clone()));
                    self.selection = None;
                    self.dragging = None;
                }
            }
            if recording {
                ui.spinner();
                if self.recorder.stopping() {
                    ui.label("正在整理录音…");
                } else if let Some(elapsed) = self.recorder.elapsed() {
                    ui.label(format!("正在录制 · {}", time(elapsed)));
                } else {
                    ui.label("正在打开默认扬声器…");
                }
            } else {
                hint(
                    ui,
                    &format!("默认扬声器 · 最长 {} 分钟", MAX_RECORD_SECONDS / 60),
                );
            }
        });
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui) {
        let saving = self.saving();
        let recording = self.recorder.activity() == Some(Activity::Recording);
        let clip = self.recorder.clip.as_ref();
        let total = clip.map_or(0, |clip| {
            clip.audio.samples.len() / clip.audio.channels as usize
        });
        let rate = clip.map_or(48_000, |clip| clip.audio.sample_rate);
        let duration = total as f64 / f64::from(rate);
        let editable = !saving && !recording && total > 0;
        let mut selection = self.selection.unwrap_or(Selection { start: 0, end: 0 });
        let mut changed = None;
        let previewing = editable && self.recorder.busy();
        let mut toggle_preview = false;
        ui.add_enabled_ui(editable, |ui| {
            changed = timeline(
                ui,
                clip.map_or(&[], |clip| clip.peaks.as_slice()),
                total,
                &mut selection,
                &mut self.dragging,
                self.recorder.preview_frame(),
            );
            ui.horizontal_wrapped(|ui| {
                let mut start = selection.start as f64 / f64::from(rate);
                let mut end = selection.end as f64 / f64::from(rate);
                ui.label("开始");
                if ui
                    .add(
                        egui::DragValue::new(&mut start)
                            .range(0.0..=selection.end.saturating_sub(1) as f64 / f64::from(rate))
                            .speed(0.01)
                            .fixed_decimals(3)
                            .suffix(" 秒"),
                    )
                    .changed()
                {
                    selection.set(
                        Edge::Left,
                        (start * f64::from(rate)).round() as usize,
                        total,
                    );
                    changed = Some(Edge::Left);
                }
                ui.label("结束");
                if ui
                    .add(
                        egui::DragValue::new(&mut end)
                            .range(
                                if total == 0 {
                                    0.0
                                } else {
                                    (selection.start + 1) as f64 / f64::from(rate)
                                }..=duration,
                            )
                            .speed(0.01)
                            .fixed_decimals(3)
                            .suffix(" 秒"),
                    )
                    .changed()
                {
                    selection.set(Edge::Right, (end * f64::from(rate)).round() as usize, total);
                    changed = Some(Edge::Right);
                }
                let label = if previewing {
                    "停止试听"
                } else {
                    "试听选中片段"
                };
                toggle_preview = ui.button(label).clicked();
            });
        });
        self.selection = Some(selection);
        if toggle_preview {
            if previewing {
                self.recorder.stop();
            } else {
                self.recorder.queue_preview(selection.range(), true);
            }
        } else if let Some(edge) = changed {
            self.recorder
                .queue_preview(selection.preview(edge, rate), false);
        }
        ui.add_space(12.0);
        ui.label("音效名称");
        ui.add_enabled(
            !saving,
            egui::TextEdit::singleline(&mut self.name)
                .desired_width(f32::INFINITY)
                .hint_text("输入名称"),
        );
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            let valid_name = is_valid_file_name(self.name.trim());
            if ui
                .add_enabled(
                    editable && selection.start < selection.end && valid_name,
                    egui::Button::new("导入音效库"),
                )
                .clicked()
            {
                self.recorder.save(selection, self.name.clone());
            }
            if saving {
                ui.spinner();
                ui.label("正在保存…");
            }
        });
        if !self.name.trim().is_empty() && !is_valid_file_name(self.name.trim()) {
            warning(ui, "名称不能包含路径、控制字符或 Windows 文件名保留字符。");
        }
    }
}

fn time(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

fn timeline(
    ui: &mut egui::Ui,
    peaks: &[f32],
    total: usize,
    selection: &mut Selection,
    dragging: &mut Option<Edge>,
    playhead: Option<usize>,
) -> Option<Edge> {
    let (outer, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 96.0),
        egui::Sense::click_and_drag(),
    );
    let rect = outer.shrink2(egui::vec2(10.0, 18.0));
    let x = |frame: usize| rect.left() + rect.width() * (frame as f64 / total.max(1) as f64) as f32;
    let mut changed = None;
    if ui.is_enabled() && total > 0 {
        if (response.drag_started() || response.clicked())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            *dragging = Some(
                if (pointer.x - x(selection.start)).abs() <= (pointer.x - x(selection.end)).abs() {
                    Edge::Left
                } else {
                    Edge::Right
                },
            );
        }
        if (response.dragged() || response.clicked())
            && let (Some(edge), Some(pointer)) = (*dragging, response.interact_pointer_pos())
        {
            let frame = (((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64
                * total as f64)
                .round() as usize;
            let previous = selection.range();
            selection.set(edge, frame, total);
            if selection.range() != previous {
                changed = Some(edge);
            }
        }
        if response.drag_stopped() || response.clicked() {
            *dragging = None;
        }
    }
    if ui.is_enabled() && total > 0 && (response.hovered() || response.dragged()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let painter = ui.painter();
    let accent = ui.visuals().selection.bg_fill;
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    if total == 0 {
        painter.line_segment(
            [rect.left_center(), rect.right_center()],
            egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        );
        return None;
    }
    let selected = egui::Rect::from_min_max(
        egui::pos2(x(selection.start), rect.top()),
        egui::pos2(x(selection.end), rect.bottom()),
    );
    painter.rect_filled(selected, 0.0, accent.gamma_multiply(0.3));
    let scale = peaks.iter().copied().fold(0.02_f32, f32::max);
    for (i, &peak) in peaks.iter().enumerate() {
        let center = (i as f32 + 0.5) / peaks.len() as f32;
        let frame = (center as f64 * total as f64) as usize;
        let height = (peak / scale).clamp(0.01, 1.0) * (rect.height() / 2.0 - 6.0);
        let px = rect.left() + center * rect.width();
        let color = if selection.range().contains(&frame) {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color().gamma_multiply(0.45)
        };
        painter.line_segment(
            [
                egui::pos2(px, rect.center().y - height),
                egui::pos2(px, rect.center().y + height),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
    for (frame, label, align) in [
        (selection.start, "开始", egui::Align2::LEFT_BOTTOM),
        (selection.end, "结束", egui::Align2::RIGHT_BOTTOM),
    ] {
        let px = x(frame);
        painter.line_segment(
            [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
            egui::Stroke::new(2.0, accent),
        );
        let handle =
            egui::Rect::from_center_size(egui::pos2(px, rect.center().y), egui::vec2(14.0, 32.0));
        painter.rect_filled(handle, 4.0, accent);
        painter.line_segment(
            [
                handle.center_top() + egui::vec2(0.0, 7.0),
                handle.center_bottom() - egui::vec2(0.0, 7.0),
            ],
            egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
        );
        painter.text(
            egui::pos2(px, rect.top() - 3.0),
            align,
            label,
            egui::TextStyle::Small.resolve(ui.style()),
            ui.visuals().text_color(),
        );
    }
    if let Some(frame) = playhead {
        painter.line_segment(
            [
                egui::pos2(x(frame), rect.top()),
                egui::pos2(x(frame), rect.bottom()),
            ],
            egui::Stroke::new(1.5, ui.visuals().warn_fg_color),
        );
    }
    changed
}
