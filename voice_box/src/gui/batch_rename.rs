use crate::{
    i18n::{Message, Text},
    soundboard::{
        library::Library,
        rename::{RenameLocation, RenamePlan, RenameRule},
        sound::SoundId,
    },
};
use std::{collections::HashSet, ops::Range};

#[derive(Clone, Copy)]
pub(super) struct Status {
    pub available: bool,
    pub busy: bool,
    pub playing: bool,
}
pub(super) enum Action {
    None,
    Closed,
    Apply(RenamePlan),
    StopPlayback,
}
#[derive(Default)]
struct Footer {
    apply: bool,
    close: bool,
    stop: bool,
}

pub(super) struct BatchRename {
    selected: HashSet<SoundId>,
    rule: RenameRule,
    plan: RenamePlan,
    error: Message,
    applying: bool,
}

impl BatchRename {
    pub fn new(library: &Library, selected: HashSet<SoundId>) -> Self {
        let rule = RenameRule::default();
        let plan = RenamePlan::new(library, &selected, &rule);
        Self {
            selected,
            rule,
            plan,
            error: Message::Empty,
            applying: false,
        }
    }

    fn rebuild(&mut self, library: &Library) {
        self.plan = RenamePlan::new(library, &self.selected, &self.rule);
        self.plan.validate(library);
    }

    fn error_text(&self) -> String {
        let mut kinds = Vec::new();
        for entry in &self.plan.entries {
            if let Some(error) = entry.error
                && !kinds.contains(&error)
            {
                kinds.push(error);
            }
        }
        let mut messages = Vec::new();
        for message in [&self.error]
            .into_iter()
            .filter(|message| !message.is_empty())
            .map(|message| message.text().into_owned())
            .chain(kinds.into_iter().map(|kind| kind.text().to_owned()))
        {
            if !messages.contains(&message) {
                messages.push(message);
            }
        }
        messages.join("；")
    }

    pub(super) fn applying(&self) -> bool {
        self.applying
    }
    pub(super) fn selected(&self) -> &HashSet<SoundId> {
        &self.selected
    }
    pub(super) fn complete(&mut self, library: &Library, result: anyhow::Result<()>) -> bool {
        self.applying = false;
        self.selected = self
            .plan
            .entries
            .iter()
            .filter_map(|entry| library.sounds.get(entry.index))
            .map(|sound| sound.id)
            .collect();
        match result {
            Ok(()) => true,
            Err(error) => {
                tracing::error!("Batch rename failed: {error:#}");
                self.rebuild(library);
                self.error = Message::error(error);
                false
            }
        }
    }
    pub(super) fn apply_result(&mut self, result: anyhow::Result<()>) {
        match result {
            Ok(()) => self.applying = true,
            Err(error) => self.error = Message::error(error),
        }
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, library: &Library, status: Status) -> Action {
        let mut apply = false;
        let mut close = false;
        let mut stop = false;
        let response = egui::Modal::new(egui::Id::unique("batch_rename"))
            .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())).inner_margin(16))
            .show(ctx, |ui| {
                let width = (ctx.content_rect().width() - 64.0).clamp(160.0, 760.0);
                let height = (ctx.content_rect().height() - 64.0).clamp(160.0, 700.0);
                let line_height = ui.text_style_height(&egui::TextStyle::Body).max(18.0);
                let preview_footer_spacing = 12.0;
                let summary_button_spacing = 12.0;
                let footer_height = line_height
                    + ui.spacing().interact_size.y
                    + ui.spacing().item_spacing.y * 2.0
                    + summary_button_spacing;
                let body_height =
                    height - footer_height - ui.spacing().item_spacing.y - preview_footer_spacing;
                ui.set_width(width);
                // Only the body may scroll; actions stay visible even in a short viewport.
                ui.scope(|ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("rename_dialog")
                        .max_height(body_height)
                        .min_scrolled_height(0.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let content_top = ui.cursor().top();
                            ui.heading(Text::BatchRename.text());
                            ui.add_space(8.0);
                            self.rule_ui(ui, library, status.available);
                            self.preview_ui(ui, body_height, content_top);
                        });
                });
                ui.add_space(preview_footer_spacing);
                let footer = self.footer_ui(ui, status, line_height, summary_button_spacing);
                apply = footer.apply;
                close = footer.close;
                stop = footer.stop;
            });
        if self.applying {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if !self.applying && (close || response.should_close()) {
            Action::Closed
        } else if apply {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            Action::Apply(RenamePlan::new(library, &self.selected, &self.rule))
        } else if stop {
            Action::StopPlayback
        } else {
            Action::None
        }
    }
    fn rule_ui(&mut self, ui: &mut egui::Ui, library: &Library, available: bool) {
        ui.add_enabled_ui(!self.applying && available, |ui| {
            let old_location = self.rule.location;
            ui.horizontal_wrapped(|ui| {
                ui.label(Text::RenameLocation.text());
                egui::ComboBox::from_id_salt("rename_location")
                    .width(ui.available_width().min(200.0))
                    .truncate()
                    .selected_text(self.rule.location.label().text())
                    .show_ui(ui, |ui| {
                        for location in [
                            RenameLocation::Start,
                            RenameLocation::End,
                            RenameLocation::Entire,
                            RenameLocation::Matches,
                        ] {
                            ui.selectable_value(
                                &mut self.rule.location,
                                location,
                                location.label().text(),
                            );
                        }
                    });
            });
            ui.add_space(6.0);
            let mut changed = old_location != self.rule.location;
            if changed {
                self.rule.content = self.rule.location.default_content();
            }
            if self.rule.location == RenameLocation::Matches {
                ui.horizontal(|ui| {
                    ui.label(Text::RenameFind.text());
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.rule.find)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                });
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                ui.label(Text::RenameContent.text());
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.rule.content)
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
            });
            if changed {
                self.error.clear();
                self.rebuild(library);
            }
        });
    }
    fn preview_ui(&self, ui: &mut egui::Ui, body_height: f32, content_top: f32) {
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.strong(Text::RenamePreview.text());
        ui.add_space(4.0);
        ui.spacing_mut().scroll.floating = false;
        let gap = ui.spacing().item_spacing.x;
        let column = (ui.available_width() - gap) / 2.0;
        ui.horizontal(|ui| {
            ui.add_sized([column, 18.0], egui::Label::new(Text::RenameOldName.text()));
            ui.add_sized([column, 18.0], egui::Label::new(Text::RenameNewName.text()));
        });
        let row_height = ui
            .text_style_height(&egui::TextStyle::Body)
            .max(ui.spacing().interact_size.y);
        egui::ScrollArea::vertical()
            .id_salt("rename_preview")
            .max_height(
                (body_height - (ui.cursor().top() - content_top) - ui.spacing().item_spacing.y)
                    .clamp(48.0, 360.0),
            )
            .min_scrolled_height(0.0)
            .auto_shrink([false, true])
            .show_rows(ui, row_height, self.plan.entries.len(), |ui, rows| {
                for row in rows {
                    let entry = &self.plan.entries[row];
                    let old = entry.original.as_str();
                    ui.push_id(row, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                // Match the height reserved by show_rows, even for short labels.
                                ui.set_min_height(row_height);
                                ui.horizontal(|ui| {
                                    let width = (ui.available_width() - gap) / 2.0;
                                    name_label(ui, width, old, &entry.old_marks, false);
                                    name_label(
                                        ui,
                                        width,
                                        &entry.name,
                                        &entry.new_marks,
                                        entry.error.is_some(),
                                    );
                                });
                            },
                        );
                    });
                }
            });
    }
    fn footer_ui(
        &self,
        ui: &mut egui::Ui,
        status: Status,
        line_height: f32,
        summary_button_spacing: f32,
    ) -> Footer {
        let mut footer = Footer::default();
        ui.scope(|ui| {
            let summary = Message::RenameSummary {
                count: self.plan.entries.len(),
                changed: self.plan.change_count(),
                errors: self.plan.error_count(),
            }
            .text();
            ui.add(
                egui::Label::new(summary.clone())
                    .truncate()
                    .show_tooltip_when_elided(false),
            )
            .on_hover_text(summary);
            ui.add_space(summary_button_spacing);
            ui.horizontal(|ui| {
                if status.playing {
                    if ui
                        .button(Text::StopPlayback.text())
                        .on_hover_text(Text::StopBeforeRename.text())
                        .clicked()
                    {
                        footer.stop = true;
                    }
                } else {
                    footer.apply = ui
                        .add_enabled(
                            status.available
                                && !status.busy
                                && !self.applying
                                && self.plan.error_count() == 0
                                && self.plan.change_count() > 0,
                            egui::Button::new(
                                if self.applying {
                                    Text::RenameApplying
                                } else {
                                    Text::ApplyRename
                                }
                                .text(),
                            ),
                        )
                        .clicked();
                }
                footer.close = ui
                    .add_enabled(!self.applying, egui::Button::new(Text::Cancel.text()))
                    .clicked();
                if self.applying {
                    ui.add(egui::Spinner::new().size(line_height));
                }
                let errors = self.error_text();
                if !errors.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(errors.replace(['\r', '\n'], " "))
                                    .color(ui.visuals().error_fg_color),
                            )
                            .truncate()
                            .show_tooltip_when_elided(false),
                        )
                        .on_hover_text(errors);
                    });
                }
            });
        });
        footer
    }
}

fn name_label(ui: &mut egui::Ui, width: f32, name: &str, marks: &[Range<usize>], error: bool) {
    let normal = if error {
        ui.visuals().error_fg_color
    } else {
        ui.visuals().text_color()
    };
    let accent = ui.visuals().selection;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    let mut end = 0;
    for range in marks {
        job.append(
            &name[end..range.start],
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: normal,
                ..Default::default()
            },
        );
        let marked = if range.is_empty() {
            "\u{202f}"
        } else {
            &name[range.clone()]
        };
        job.append(
            marked,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: if error { normal } else { accent.stroke.color },
                background: if error {
                    normal.gamma_multiply(0.12)
                } else {
                    accent.bg_fill
                },
                ..Default::default()
            },
        );
        end = range.end;
    }
    job.append(
        &name[end..],
        0.0,
        egui::TextFormat {
            font_id: font,
            color: normal,
            ..Default::default()
        },
    );
    ui.add_sized(
        [width, ui.text_style_height(&egui::TextStyle::Body)],
        egui::Label::new(job.clone())
            .truncate()
            .show_tooltip_when_elided(false),
    )
    .on_hover_ui(|ui| {
        ui.set_max_width(700.0);
        ui.label(job);
    });
}
