//! Left sidebar: Scenes (switch, rename inline, reorder) and Layers (the current scene's
//! items, top of the stack first, with visibility / lock toggles and an add menu).

use eframe::egui::{self, vec2};
use sphere_ui::{self as sui, Icon};

use super::preview::ItemAction;
use super::{Rename, RenameTarget, StudioApp};
use crate::model::{Id, SourceKind};

impl StudioApp {
    pub(crate) fn left_sidebar(&mut self, ui: &mut egui::Ui) {
        let h = ui.available_height();
        let scenes_h = (h * 0.38).clamp(150.0, 320.0);
        ui.allocate_ui(vec2(ui.available_width(), scenes_h), |ui| self.scenes_panel(ui));
        ui.add_space(sui::space::BASE);
        ui.allocate_ui(vec2(ui.available_width(), ui.available_height()), |ui| self.layers_panel(ui));
    }

    fn scenes_panel(&mut self, ui: &mut egui::Ui) {
        let count = self.col.scenes.len();
        let mut add = false;
        let mut dup = None;
        let mut remove = None;
        let mut rename = None;
        let mut switch = None;
        let mut moves: Option<(usize, isize)> = None;
        let cur = self.col.current_scene;
        let cur_idx = self.col.scenes.iter().position(|s| s.id == cur).unwrap_or(0);
        let scenes: Vec<(Id, String, usize)> = self.col.scenes.iter().map(|s| (s.id, s.name.clone(), s.items.len())).collect();
        let mut start_rename: Option<(Id, String)> = None;
        sui::Panel::new(tr!("Scenes")).icon(Icon::Clapperboard).subtitle(count.to_string()).fill_height(true).show(
            ui,
            |ui| {
                if ui.add(sui::IconButton::new(Icon::Plus).tooltip(tr!("New scene"))).clicked() {
                    add = true;
                }
                if ui.add(sui::IconButton::new(Icon::ChevronDown).tooltip(tr!("Move down")).enabled(cur_idx + 1 < count)).clicked() {
                    moves = Some((cur_idx, 1));
                }
                if ui.add(sui::IconButton::new(Icon::ChevronUp).tooltip(tr!("Move up")).enabled(cur_idx > 0)).clicked() {
                    moves = Some((cur_idx, -1));
                }
            },
            |ui| {
                egui::ScrollArea::vertical().id_salt("scenes").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = sui::space::HAIR;
                    for (id, name, n_items) in &scenes {
                        let id = *id;
                        let editing = self.rename.as_ref().is_some_and(|r| r.target == RenameTarget::Scene(id));
                        if editing {
                            let r = self.rename.as_mut().unwrap();
                            let resp = ui.add(egui::TextEdit::singleline(&mut r.text).desired_width(f32::INFINITY));
                            if r.focus {
                                resp.request_focus();
                                r.focus = false;
                            }
                            if resp.lost_focus() {
                                rename = Some((id, ui.input(|i| !i.key_pressed(egui::Key::Escape))));
                            }
                            continue;
                        }
                        let on = id == cur;
                        let row = sui::ListRow::new(name)
                            .icon(if on { Icon::MonitorPlay } else { Icon::SquareStack })
                            .selected(on)
                            .meta(format!("{n_items}"))
                            .show(ui, |ui| {
                                if !on {
                                    ui.add_space(1.0);
                                }
                            });
                        if row.clicked() {
                            switch = Some(id);
                        }
                        if row.double_clicked() {
                            start_rename = Some((id, name.clone()));
                        }
                        row.context_menu(|ui| {
                            if sui::menu_item(ui, Some(Icon::Pencil), tr!("Rename"), None).clicked() {
                                start_rename = Some((id, name.clone()));
                                ui.close_menu();
                            }
                            if sui::menu_item(ui, Some(Icon::CopyPlus), tr!("Duplicate"), None).clicked() {
                                dup = Some(id);
                                ui.close_menu();
                            }
                            ui.separator();
                            let p = sui::palette();
                            if sui::menu_item_colored(ui, Some(Icon::Trash2), tr!("Remove"), None, Some(p.status_error), count > 1).clicked() {
                                remove = Some(id);
                                ui.close_menu();
                            }
                        });
                    }
                });
            },
        );
        if add {
            let id = self.col.add_scene(tr!("Scene"));
            self.col.current_scene = id;
            self.select_item(None);
        }
        if let Some((i, d)) = moves {
            let j = (i as isize + d) as usize;
            if j < self.col.scenes.len() {
                self.col.scenes.swap(i, j);
            }
        }
        if let Some(id) = switch {
            if id != self.col.current_scene {
                self.col.current_scene = id;
                self.select_item(None);
            }
        }
        if let Some((id, commit)) = rename {
            if let Some(r) = self.rename.take() {
                let name = r.text.trim().to_string();
                if commit && !name.is_empty() {
                    if self.col.scenes.iter().any(|s| s.id != id && s.name == name) {
                        self.notify(trf!("A scene named \"{name}\" already exists", name), true);
                    } else if let Some(s) = self.col.scene_mut(id) {
                        s.name = name;
                    }
                }
            }
        }
        if let Some((id, name)) = start_rename {
            self.rename = Some(Rename { target: RenameTarget::Scene(id), text: name, focus: true });
        }
        if let Some(id) = dup {
            self.duplicate_scene(id);
        }
        if let Some(id) = remove {
            self.remove_scene(id);
        }
    }

    fn remove_scene(&mut self, id: Id) {
        if self.col.scenes.len() <= 1 {
            self.notify(tr!("A collection needs at least one scene"), true);
            return;
        }
        let idx = self.col.scenes.iter().position(|s| s.id == id);
        self.col.scenes.retain(|s| s.id != id);
        if self.col.current_scene == id {
            let i = idx.unwrap_or(0).min(self.col.scenes.len() - 1);
            self.col.current_scene = self.col.scenes[i].id;
        }
        self.col.prune_orphans();
        self.select_item(None);
    }

    fn duplicate_scene(&mut self, id: Id) {
        let Some(src) = self.col.scene(id).cloned() else { return };
        let new_id = self.col.add_scene(&src.name);
        let items: Vec<_> = src
            .items
            .iter()
            .map(|i| {
                let mut i = i.clone();
                i.id = self.col.alloc_id();
                i
            })
            .collect();
        if let Some(s) = self.col.scene_mut(new_id) {
            s.items = items;
        }
    }

    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        let items: Vec<_> = self.col.scene(self.col.current_scene).map(|s| s.items.clone()).unwrap_or_default();
        let rows: Vec<(crate::model::SceneItem, String, Icon, Option<String>)> = items
            .iter()
            .rev()
            .filter_map(|it| {
                let src = self.col.source(it.source)?;
                let err = self.engine.runtime(it.source).and_then(|r| r.shared.error());
                Some((it.clone(), src.name.clone(), src.kind.icon(), err))
            })
            .collect();
        let in_scene: std::collections::HashSet<Id> = items.iter().map(|i| i.source).collect();
        let existing: Vec<(Id, String, Icon)> = self
            .col
            .sources
            .iter()
            .filter(|s| !in_scene.contains(&s.id) && !self.col.global_audio.contains(&s.id))
            .map(|s| (s.id, s.name.clone(), s.kind.icon()))
            .collect();
        let mut add_kind: Option<SourceKind> = None;
        let mut add_existing: Option<Id> = None;
        let mut hdr_remove: Option<Id> = None;
        let mut hdr_kind: Option<SourceKind> = None;
        let mut actions: Vec<(Id, ItemAction)> = Vec::new();
        let sel = self.selected_item;
        sui::Panel::new(tr!("Layers")).icon(Icon::Layers).subtitle(rows.len().to_string()).fill_height(true).show(
            ui,
            |ui| {
                let p = sui::palette();
                if ui.add(sui::IconButton::new(Icon::Trash2).tooltip(tr!("Remove layer")).enabled(sel.is_some()).color(if sel.is_some() { p.text_muted } else { p.text_disabled })).clicked() {
                    if let Some(id) = sel {
                        hdr_remove = Some(id);
                    }
                }
                let add = ui.add(sui::IconButton::new(Icon::Plus).tooltip(tr!("Add layer")));
                sui::popover(ui, &add, egui::Id::new("add-layer"), |ui| {
                    sui::menu_header(ui, tr!("New source"));
                    for kind in SourceKind::templates() {
                        if sui::menu_item(ui, Some(kind.icon()), kind.type_name(), None).clicked() {
                            hdr_kind = Some(kind);
                        }
                    }
                    if !existing.is_empty() {
                        ui.separator();
                        sui::menu_header(ui, tr!("Existing source"));
                        for (id, name, icon) in &existing {
                            if sui::menu_item(ui, Some(*icon), name, None).clicked() {
                                add_existing = Some(*id);
                            }
                        }
                    }
                });
            },
            |ui| {
                if rows.is_empty() {
                    sui::empty_state(ui, Icon::Layers, tr!("No layers yet"), tr!("Add a display, camera, media file, text …"));
                    ui.vertical_centered(|ui| {
                        let b = ui.add(sui::Button::primary(tr!("Add Layer")).icon(Icon::Plus));
                        sui::popover(ui, &b, egui::Id::new("add-layer-empty"), |ui| {
                            for kind in SourceKind::templates() {
                                if sui::menu_item(ui, Some(kind.icon()), kind.type_name(), None).clicked() {
                                    add_kind = Some(kind);
                                }
                            }
                        });
                    });
                    return;
                }
                egui::ScrollArea::vertical().id_salt("layers").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = sui::space::HAIR;
                    let p = sui::palette();
                    for (item, name, icon, err) in &rows {
                        let selected = self.selected_item == Some(item.id);
                        let mut row = sui::ListRow::new(name)
                            .icon(if err.is_some() { Icon::TriangleAlert } else { *icon })
                            .selected(selected)
                            .dimmed(!item.visible)
                            .warning(err.is_some())
                            .show(ui, |ui| {
                                let lock = sui::IconButton::new(if item.locked { Icon::Lock } else { Icon::LockOpen })
                                    .size(22.0)
                                    .tooltip(if item.locked { tr!("Unlock") } else { tr!("Lock") })
                                    .color(if item.locked { p.status_warning } else { p.text_faint });
                                if ui.add(lock).clicked() {
                                    actions.push((item.id, ItemAction::ToggleLock));
                                }
                                let eye = sui::IconButton::new(if item.visible { Icon::Eye } else { Icon::EyeOff })
                                    .size(22.0)
                                    .tooltip(if item.visible { tr!("Hide") } else { tr!("Show") })
                                    .color(if item.visible { p.text_muted } else { p.text_faint });
                                if ui.add(eye).clicked() {
                                    actions.push((item.id, ItemAction::ToggleVisible));
                                }
                            });
                        if let Some(e) = err {
                            row = row.on_hover_text(e.as_str());
                        }
                        if row.clicked() || row.secondary_clicked() {
                            self.select_item(Some(item.id));
                        }
                        if row.double_clicked() {
                            self.select_item(Some(item.id));
                            self.open_properties(item.source);
                        }
                        row.context_menu(|ui| self.item_menu(ui, item.id));
                    }
                });
            },
        );
        if let Some(id) = hdr_remove {
            actions.push((id, ItemAction::Remove));
        }
        if hdr_kind.is_some() {
            add_kind = hdr_kind;
        }
        for (id, a) in actions {
            self.item_action(id, a);
        }
        let scene = self.col.current_scene;
        if let Some(kind) = add_kind {
            let source = self.col.add_source(kind.type_name(), kind);
            if let Some(item) = self.col.add_item(scene, source) {
                self.select_item(Some(item));
            }
            self.open_properties(source);
        }
        if let Some(source) = add_existing {
            if let Some(item) = self.col.add_item(scene, source) {
                self.select_item(Some(item));
            }
        }
    }
}
