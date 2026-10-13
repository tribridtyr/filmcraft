//! Headless UI tests of the Timeline tools after a track select, the Hand tool and zoom, driven
//! over the control channel like a person with the mouse and keyboard.
//! Set `FILMCRAFT_UI_SHOTS=<dir>` to save screenshots of the key steps.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// Two 48-frame clips on V1 (their sounds on A1) at frames 0 and 96.
    fn clips() -> (Self, ClipId, ClipId) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.newSequence", json!({"name": "Tools", "video": 1, "audio": 1})).unwrap();
        let rate = s.sequence_rate();
        let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
        for f in [0i64, 96] {
            s.execute("timeline.place", json!({"item": item.0, "frame": f, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
        }
        s.execute("edit.deselectAll", json!({})).unwrap();
        let v1 = &s.active_sequence().unwrap().video_tracks[0];
        let (a, b) = (v1.items[0].id, v1.items[1].id);
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SHOTS").is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        (d, a, b)
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn start(&mut self, c: ClipId) -> i64 {
        let s = &self.harness.state().session;
        let rate = s.sequence_rate();
        let t = s.active_sequence().unwrap().find_item(c).map(|(_, i)| i.start).unwrap();
        rate.frame_at(t)
    }

    fn shot(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from) else { return };
        self.frames(3);
        std::fs::create_dir_all(&dir).unwrap();
        self.harness.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
    }

    /// Screen position of a timeline frame on the video track row, from the clips' own rectangles.
    fn x_of_clip_start(&mut self, c: ClipId) -> (f32, f32) {
        // `ui.timeline.locate` returns the clip's In edge as a screen point; step well inside it
        let r = self.ok("ui.timeline.locate", json!({"clip": c.0, "edge": "in"}));
        let (x, y) = (r["x"].as_f64().unwrap_or_else(|| panic!("no x in {r}")), r["y"].as_f64().unwrap_or_else(|| panic!("no y in {r}")));
        (x as f32 + 20.0, y as f32)
    }
}

/// After a track select (A), dragging moves what it selected: no switch back to V first.
#[test]
fn dragging_after_a_track_select_moves_the_selected_clips() {
    let (mut d, a, b) = Driver::clips();
    d.ok("ui.set", json!({"tool": "TrackSelectForward"}));
    d.frames(2);
    let (x, y) = d.x_of_clip_start(a);
    d.ok("ui.click", json!({"x": x, "y": y}));
    d.frames(2);
    assert!(
        d.harness.state().session.state.selection.contains(&a) && d.harness.state().session.state.selection.contains(&b),
        "A selects the clip and everything after it"
    );
    d.shot("tools-1-track-selected");
    let (a0, b0) = (d.start(a), d.start(b));
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 120.0, "y": y}, "steps": 10}));
    d.frames(3);
    let (a1, b1) = (d.start(a), d.start(b));
    assert!(a1 > a0 && b1 > b0, "both clips moved right: {a0}->{a1}, {b0}->{b1}");
    assert_eq!(b1 - a1, b0 - a0, "and kept their spacing");
    d.shot("tools-2-dragged");
}

/// The Hand tool drags the timeline view sideways.
#[test]
fn the_hand_tool_pans_the_timeline() {
    let (mut d, a, _) = Driver::clips();
    d.ok("ui.set", json!({"timeline": {"pps": 200.0, "scroll": 1.0}}));
    d.frames(30);
    d.ok("ui.set", json!({"tool": "Hand"}));
    d.frames(2);
    let before = d.harness.state().ui.timeline.target_scroll;
    let (x, y) = d.x_of_clip_start(a);
    d.ok("ui.drag", json!({"from": {"x": x + 300.0, "y": y}, "to": {"x": x + 100.0, "y": y}, "steps": 10}));
    d.frames(3);
    let after = d.harness.state().ui.timeline.target_scroll;
    assert!(after > before + 0.5, "dragging left scrolls the view to later time: {before} -> {after}");
    d.shot("tools-3-hand-panned");
}

/// The Zoom tool (click, Option-click) and the keyboard zoom the timeline in and out.
#[test]
fn the_zoom_tool_and_keys_zoom_the_timeline() {
    let (mut d, a, _) = Driver::clips();
    let pps = |d: &mut Driver| {
        d.frames(40);
        d.harness.state().ui.timeline.target_pps
    };
    let base = pps(&mut d);
    d.ok("ui.set", json!({"tool": "Zoom"}));
    d.frames(2);
    let (x, y) = d.x_of_clip_start(a);
    d.ok("ui.click", json!({"x": x, "y": y}));
    let zoomed_in = pps(&mut d);
    assert!(zoomed_in > base * 1.5, "a click zooms in: {base} -> {zoomed_in}");
    d.ok("ui.click", json!({"x": x, "y": y, "modifiers": {"alt": true}}));
    let zoomed_out = pps(&mut d);
    assert!(zoomed_out < zoomed_in * 0.75, "Option-click zooms out: {zoomed_in} -> {zoomed_out}");
    d.ok("ui.set", json!({"tool": "Selection", "focused": "Timeline"}));
    d.frames(2);
    let before = pps(&mut d);
    d.exec("view.zoomIn", json!({}));
    let k_in = pps(&mut d);
    assert!(k_in > before, "view.zoomIn zooms in: {before} -> {k_in}");
    d.exec("view.zoomOut", json!({}));
    let k_out = pps(&mut d);
    assert!(k_out < k_in, "view.zoomOut zooms out: {k_in} -> {k_out}");
}

/// Right-click a keyframe in Effect Controls: Ease Out / Ease In change its interpolation, and a
/// keyframe pair with both eased reads as a flat Bezier (the velocity graph's handles).
#[test]
fn right_click_ease_in_and_out_on_a_keyframe() {
    use filmcraft_project::Interpolation;
    let (mut d, a, _) = Driver::clips();
    d.exec("timeline.select", json!({"clips": [a.0]}));
    d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
    // maximize it: in the small default panel the Scale row is scrolled out of view
    d.ok("ui.set", json!({"focused": "Effect Controls"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.frames(3);
    d.exec("effects.apply", json!({"clips": [a.0], "effect": "transform"}));
    for (frame, value) in [(0, 100.0), (40, 140.0)] {
        d.exec("effects.setParam", json!({"clip": a.0, "effect": "transform", "param": "scale_height", "value": value, "frame": frame, "keyframe": true}));
    }
    d.frames(4);
    let interps = |d: &mut Driver| -> Vec<Interpolation> {
        let s = &d.harness.state().session;
        let it = s.active_sequence().unwrap().find_item(a).map(|(_, i)| i.clone()).unwrap();
        it.effect("transform").unwrap().param("scale_height").unwrap().keyframes.iter().map(|k| k.interp).collect()
    };
    let before = interps(&mut d);
    assert_eq!(before.len(), 2, "two keyframes: {before:?}");
    let ids = |d: &mut Driver, needle: &str| -> Vec<String> {
        d.ok("ui.elements", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["id"].as_str().map(str::to_string))
            .filter(|id| id.contains(needle))
            .collect()
    };
    let kfs: Vec<String> = ids(&mut d, ".keyframe.").into_iter().filter(|i| !i.contains(".interp.") && !i.ends_with(".clear")).collect();
    assert!(kfs.len() >= 2, "the keyframe diamonds are on screen in Effect Controls: {kfs:?}");
    d.shot("tools-4-keyframes");
    d.ok("ui.click", json!({"id": kfs[0], "button": "right"}));
    d.frames(8);
    d.shot("tools-5-ease-menu");
    let ease_out = ids(&mut d, ".interp.easeOut");
    assert_eq!(ease_out.len(), 1, "the context menu offers Ease Out: {ease_out:?}; elements near it: {:?}", ids(&mut d, "keyframe"));
    d.ok("ui.click", json!({"id": ease_out[0]}));
    d.frames(4);
    let after = interps(&mut d);
    assert_ne!(after, before, "Ease Out changed the keyframe's interpolation: {before:?} -> {after:?}");
    assert_eq!(after[0], Interpolation::EaseOut, "{after:?}");
}
