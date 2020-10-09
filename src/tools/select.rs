use std::collections::BTreeSet;
use std::sync::Arc;

use druid::kurbo::{Point, Rect, Vec2};
use druid::piet::{Color, RenderContext};
use druid::{Data, Env, EventCtx, HotKey, KbKey, KeyEvent, MouseEvent, PaintCtx, RawMods};

use crate::design_space::{DPoint, DVec2};
use crate::edit_session::EditSession;
use crate::mouse::{Drag, Mouse, MouseDelegate, TaggedEvent};
use crate::path::{EntityId, PathSeg};
use crate::tools::{EditType, Tool, ToolId};

const SELECTION_RECT_BG_COLOR: Color = Color::rgba8(0xDD, 0xDD, 0xDD, 0x55);
const SELECTION_RECT_STROKE_COLOR: Color = Color::rgb8(0x53, 0x8B, 0xBB);

#[derive(Debug, Clone)]
enum DragState {
    /// State for a drag that is a rectangular selection.
    Select {
        previous: Arc<BTreeSet<EntityId>>,
        rect: Rect,
    },
    /// State for a drag that is moving a selected object.
    Move {
        last_used_pos: DPoint,
    },
    None,
}

/// The state of the selection tool.
#[derive(Debug, Default, Clone)]
pub struct Select {
    /// the state preserved between drag events.
    drag: DragState,
    last_pos: Point,
    /// The edit type produced by the current event, if any.
    ///
    /// This is stashed here because we can't return anything from the methods in
    /// `MouseDelegate`.
    ///
    /// It is an invariant that this is always `None`, except while we are in
    /// a `key_down`, `key_up`, or `mouse_event` method.
    this_edit_type: Option<EditType>,
}

impl Tool for Select {
    fn paint(&mut self, ctx: &mut PaintCtx, _data: &EditSession, _env: &Env) {
        if let DragState::Select { rect, .. } = self.drag {
            ctx.fill(rect, &SELECTION_RECT_BG_COLOR);
            ctx.stroke(rect, &SELECTION_RECT_STROKE_COLOR, 1.0);
        }
    }

    fn key_down(
        &mut self,
        event: &KeyEvent,
        _ctx: &mut EventCtx,
        data: &mut EditSession,
        _: &Env,
    ) -> Option<EditType> {
        assert!(self.this_edit_type.is_none());
        match event {
            e if e.key == KbKey::ArrowLeft
                || e.key == KbKey::ArrowDown
                || e.key == KbKey::ArrowUp
                || e.key == KbKey::ArrowRight =>
            {
                self.nudge(data, event);
            }
            e if e.key == KbKey::Backspace => {
                data.delete_selection();
                self.this_edit_type = Some(EditType::Normal);
            }
            e if HotKey::new(None, KbKey::Tab).matches(e) => data.select_next(),
            //TODO: add Shift to SysMods
            e if HotKey::new(RawMods::Shift, KbKey::Tab).matches(e) => data.select_prev(),
            _ => return None,
        }
        self.this_edit_type.take()
    }

    fn mouse_event(
        &mut self,
        event: TaggedEvent,
        mouse: &mut Mouse,
        ctx: &mut EventCtx,
        data: &mut EditSession,
        _: &Env,
    ) -> Option<EditType> {
        assert!(self.this_edit_type.is_none());
        let pre_rect = self.drag.drag_rect();
        mouse.mouse_event(event, data, self);
        if !pre_rect.same(&self.drag.drag_rect()) {
            ctx.request_paint();
        }
        self.this_edit_type.take()
    }

    fn name(&self) -> ToolId {
        "Select"
    }
}

impl Select {
    fn nudge(&mut self, data: &mut EditSession, event: &KeyEvent) {
        let (mut nudge, edit_type) = match event.key {
            KbKey::ArrowLeft => (Vec2::new(-1.0, 0.), EditType::NudgeLeft),
            KbKey::ArrowRight => (Vec2::new(1.0, 0.), EditType::NudgeRight),
            KbKey::ArrowUp => (Vec2::new(0.0, 1.0), EditType::NudgeUp),
            KbKey::ArrowDown => (Vec2::new(0.0, -1.0), EditType::NudgeDown),
            _ => unreachable!(),
        };

        if event.mods.meta() {
            nudge *= 100.;
        } else if event.mods.shift() {
            nudge *= 10.;
        }

        data.nudge_selection(DVec2::from_raw(nudge));

        // for the purposes of undo, we only combine single-unit nudges
        if nudge.hypot().abs() > 1.0 {
            self.this_edit_type = Some(EditType::Normal);
        } else {
            self.this_edit_type = Some(edit_type);
        }
    }
}

impl MouseDelegate<EditSession> for Select {
    fn mouse_moved(&mut self, event: &MouseEvent, _data: &mut EditSession) {
        self.last_pos = event.pos;
    }

    fn left_down(&mut self, event: &MouseEvent, data: &mut EditSession) {
        if event.count == 1 {
            let sel = data.hit_test_all(event.pos, None);
            if let Some(point_id) = sel {
                if !event.mods.shift() {
                    // when clicking a point, if it is not selected we set it as the selection,
                    // otherwise we keep the selection intact for a drag.
                    if !data.selection.contains(&point_id) {
                        data.set_selection_one(point_id);
                    }
                } else if !data.selection_mut().remove(&point_id) {
                    data.selection_mut().insert(point_id);
                }
            } else if let Some((seg, _t)) = data.hit_test_segments(event.pos, None) {
                // TODO: make these non-draggable.
                if event.mods.alt() && matches!(seg, PathSeg::Line(..)) {
                    let path = data.path_for_point_mut(seg.start_id()).unwrap();
                    self.this_edit_type = Some(EditType::Normal);
                    path.upgrade_line_seg(seg);
                    return;
                }
                let ids = seg.ids();
                let sel = data.selection_mut();
                if !event.mods.shift() {
                    if !ids.iter().all(|id| sel.contains(id)) {
                        sel.clear();
                        sel.extend(&ids);
                    }
                } else if ids.iter().all(|id| sel.contains(id)) {
                    for id in &ids {
                        sel.remove(id);
                    }
                } else {
                    sel.extend(&ids);
                }
            } else if !event.mods.shift() {
                data.selection_mut().clear();
            }
        } else if event.count == 2 {
            let sel = data.hit_test_all(event.pos, None);
            match sel {
                Some(id)
                    if data
                        .path_point_for_id(id)
                        .map(|p| p.is_on_curve())
                        .unwrap_or(false) =>
                {
                    data.toggle_selected_on_curve_type();
                    self.this_edit_type = Some(EditType::Normal);
                }
                Some(id) if id.is_guide() => {
                    data.toggle_guide(id, event.pos);
                    self.this_edit_type = Some(EditType::Normal);
                }
                _ => {
                    data.select_path(event.pos, event.mods.shift());
                }
            }
        }
    }

    fn left_up(&mut self, _event: &MouseEvent, _data: &mut EditSession) {
        self.drag = DragState::None;
    }

    fn left_drag_began(&mut self, drag: Drag, data: &mut EditSession) {
        // if we're starting a rectangular selection, we save the previous selection
        let sel = data.hit_test_all(drag.start.pos, None);
        let is_dragging_item = sel.is_some();
        self.drag = if is_dragging_item {
            DragState::Move {
                last_used_pos: data.viewport.from_screen(drag.start.pos),
            }
        } else {
            DragState::Select {
                previous: data.selection.clone(),
                rect: Rect::from_points(drag.start.pos, drag.current.pos),
            }
        }
    }

    fn left_drag_changed(&mut self, drag: Drag, data: &mut EditSession) {
        match &mut self.drag {
            DragState::Select {
                previous,
                ref mut rect,
            } => {
                *rect = Rect::from_points(drag.current.pos, drag.start.pos);
                update_selection_for_drag(data, previous, *rect, drag.current.mods.shift());
            }
            DragState::Move {
                ref mut last_used_pos,
            } => {
                let drag_pos = data.viewport.from_screen(drag.current.pos);
                let drag_delta = drag_pos - *last_used_pos;
                if drag_delta.hypot() > 0. {
                    data.nudge_selection(drag_delta);
                    *last_used_pos = drag_pos;
                }
            }
            DragState::None => unreachable!("invalid state"),
        }

        if self.drag.is_move() {
            self.this_edit_type = Some(EditType::Drag);
        }
    }

    fn left_drag_ended(&mut self, _drag: Drag, _data: &mut EditSession) {
        if self.drag.is_move() {
            self.this_edit_type = Some(EditType::DragUp);
        }
    }

    fn cancel(&mut self, data: &mut EditSession) {
        let old_state = std::mem::replace(&mut self.drag, DragState::None);
        if let DragState::Select { previous, .. } = old_state {
            data.selection = previous;
        }
    }
}

fn update_selection_for_drag(
    data: &mut EditSession,
    prev_sel: &BTreeSet<EntityId>,
    rect: Rect,
    shift: bool,
) {
    let in_select_rect = data
        .iter_points()
        .filter(|p| rect.contains(p.to_screen(data.viewport)))
        .map(|p| p.id)
        .collect();
    let new_sel = if shift {
        prev_sel
            .symmetric_difference(&in_select_rect)
            .copied()
            .collect()
    } else {
        prev_sel.union(&in_select_rect).copied().collect()
    };
    *data.selection_mut() = new_sel;
}

impl Default for DragState {
    fn default() -> Self {
        DragState::None
    }
}

impl DragState {
    fn drag_rect(&self) -> Option<Rect> {
        if let DragState::Select { rect, .. } = self {
            Some(*rect)
        } else {
            None
        }
    }

    fn is_move(&self) -> bool {
        matches!(self, DragState::Move { .. })
    }
}
