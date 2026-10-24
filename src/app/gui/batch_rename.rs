use crate::app::{
    i18n::{Message, Text},
    soundboard::{
        Soundboard,
        library::Library,
        rename::{DirectorySnapshot, RenameLocation, RenamePlan, RenameRule},
    },
};
use std::{
    collections::HashSet,
    ops::Range,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) struct BatchRename {
    selected: HashSet<PathBuf>,
    rule: RenameRule,
    plan: RenamePlan,
    snapshot: Option<DirectorySnapshot>,
    scan: Option<mpsc::Receiver<anyhow::Result<Option<DirectorySnapshot>>>>,
    next_check: Instant,
    scan_error: Message,
    error: Message,
    applying: bool,
}

impl BatchRename {
    pub fn new(ctx: &egui::Context, library: &Library, selected: HashSet<PathBuf>) -> Self {
        let rule = RenameRule::default();
        let plan = RenamePlan::new(library, &selected, &rule);
        let mut tool = Self {
            selected,
            rule,
            plan,
            snapshot: None,
            scan: None,
            next_check: Instant::now(),
            scan_error: Message::Empty,
            error: Message::Empty,
            applying: false,
        };
        tool.check_directory(ctx);
        tool
    }

    fn rebuild(&mut self, library: &Library) {
        self.plan = RenamePlan::new(library, &self.selected, &self.rule);
        if let Some(snapshot) = &self.snapshot {
            self.plan.validate(library, snapshot);
        }
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
        for message in [&self.scan_error, &self.error]
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

    fn check_directory(&mut self, ctx: &egui::Context) {
        self.next_check = Instant::now() + Duration::from_secs(2);
        let Some(directory) = self
            .plan
            .entries
            .first()
            .and_then(|entry| entry.original.parent())
        else {
            self.scan_error = Text::SoundNotFound.into();
            return;
        };
        let directory = directory.to_path_buf();
        let modified = self.snapshot.as_ref().map(|snapshot| snapshot.modified);
        let (sender, receiver) = mpsc::channel();
        let wake = super::wake_ui(ctx.clone());
        match std::thread::Builder::new()
            .name("VoiceBoxRenameScan".into())
            .spawn(move || {
                let _ = sender.send(DirectorySnapshot::read_if_changed(&directory, modified));
                wake();
            }) {
            Ok(_) => self.scan = Some(receiver),
            Err(error) => self.scan_error = Message::error(error),
        }
    }

    // Returns true when the dialog can close. Selection follows the actual paths, even after rollback errors.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        model: &mut Soundboard,
        selected: &mut HashSet<PathBuf>,
        available: bool,
    ) -> bool {
        if self.applying
            && let Some(result) = model.take_rename_result()
        {
            self.applying = false;
            *selected = self
                .plan
                .entries
                .iter()
                .filter_map(|entry| model.library().sounds.get(entry.index))
                .map(|sound| sound.path.clone())
                .collect();
            self.selected.clone_from(selected);
            match result {
                Ok(()) => return true,
                Err(error) => {
                    tracing::error!("Batch rename failed: {error:#}");
                    self.snapshot = None;
                    self.scan = None;
                    self.rebuild(model.library());
                    self.check_directory(ctx);
                    self.error = Message::error(error);
                }
            }
        }
        if let Some(scan) = &self.scan {
            match scan.try_recv() {
                Ok(result) => {
                    self.scan = None;
                    match result {
                        Ok(Some(snapshot)) => {
                            self.scan_error.clear();
                            self.snapshot = Some(snapshot);
                            self.rebuild(model.library());
                        }
                        Ok(None) => self.scan_error.clear(),
                        Err(error) => {
                            self.snapshot = None;
                            self.scan_error = Message::error(error);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scan = None;
                    self.snapshot = None;
                    self.scan_error = Text::RenameThreadExited.into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.applying && self.scan.is_none() && Instant::now() >= self.next_check {
            self.check_directory(ctx);
        }
        let mut apply = false;
        let mut close = false;
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
                                    if self.snapshot.is_some() {
                                        self.error.clear();
                                    }
                                    self.rebuild(model.library());
                                }
                            });
                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);
                            ui.strong(Text::RenamePreview.text());
                            ui.add_space(4.0);
                            ui.spacing_mut().scroll.floating = false;
                            let gap = ui.spacing().item_spacing.x;
                            let column = (ui.available_width() - gap) / 2.0;
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [column, 18.0],
                                    egui::Label::new(Text::RenameOldName.text()),
                                );
                                ui.add_sized(
                                    [column, 18.0],
                                    egui::Label::new(Text::RenameNewName.text()),
                                );
                            });
                            let row_height = ui
                                .text_style_height(&egui::TextStyle::Body)
                                .max(ui.spacing().interact_size.y);
                            egui::ScrollArea::vertical()
                                .id_salt("rename_preview")
                                .max_height(
                                    (body_height
                                        - (ui.cursor().top() - content_top)
                                        - ui.spacing().item_spacing.y)
                                        .clamp(48.0, 360.0),
                                )
                                .min_scrolled_height(0.0)
                                .auto_shrink([false, true])
                                .show_rows(ui, row_height, self.plan.entries.len(), |ui, rows| {
                                    for row in rows {
                                        let entry = &self.plan.entries[row];
                                        let old =
                                            entry.original.file_name().unwrap().to_string_lossy();
                                        ui.push_id(row, |ui| {
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(ui.available_width(), row_height),
                                                egui::Layout::top_down(egui::Align::Min),
                                                |ui| {
                                                    // Match the height reserved by show_rows, even for short labels.
                                                    ui.set_min_height(row_height);
                                                    ui.horizontal(|ui| {
                                                        let width =
                                                            (ui.available_width() - gap) / 2.0;
                                                        name_label(
                                                            ui,
                                                            width,
                                                            &old,
                                                            &entry.old_marks,
                                                            false,
                                                        );
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
                        });
                });
                ui.add_space(preview_footer_spacing);
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
                        if model.playing() {
                            if ui
                                .button(Text::StopPlayback.text())
                                .on_hover_text(Text::StopBeforeRename.text())
                                .clicked()
                            {
                                model.stop();
                            }
                        } else {
                            apply = ui
                                .add_enabled(
                                    available
                                        && !model.busy()
                                        && !self.applying
                                        && self.snapshot.is_some()
                                        && self.scan_error.is_empty()
                                        && self.plan.error_count() == 0
                                        && self.plan.change_count() > 0,
                                    egui::Button::new(
                                        if self.applying {
                                            Text::RenameApplying
                                        } else if self.snapshot.is_none() && self.scan.is_some() {
                                            Text::RenameChecking
                                        } else {
                                            Text::ApplyRename
                                        }
                                        .text(),
                                    ),
                                )
                                .clicked();
                        }
                        close = ui
                            .add_enabled(!self.applying, egui::Button::new(Text::Cancel.text()))
                            .clicked();
                        if self.applying || (self.snapshot.is_none() && self.scan.is_some()) {
                            ui.add(egui::Spinner::new().size(line_height));
                        }
                        let errors = self.error_text();
                        if !errors.is_empty() {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(errors.replace(['\r', '\n'], " "))
                                                .color(ui.visuals().error_fg_color),
                                        )
                                        .truncate()
                                        .show_tooltip_when_elided(false),
                                    )
                                    .on_hover_text(errors);
                                },
                            );
                        }
                    });
                });
            });
        if apply {
            // Rebuild once more from the current rule; the worker re-scans before touching files.
            let plan = RenamePlan::new(model.library(), &self.selected, &self.rule);
            match model.rename_sounds(plan, super::wake_ui(ctx.clone())) {
                Ok(()) => self.applying = true,
                Err(error) => self.error = Message::error(error),
            }
        }
        if self.scan.is_some() || self.applying {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
            ctx.request_repaint_after(self.next_check.saturating_duration_since(Instant::now()));
        }
        !self.applying && (close || response.should_close())
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
