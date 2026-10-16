//! Headless UI test of editing the transcript in place (Premiere's Edit Segment): Return or a
//! double click on a word opens a text field, typing a correction and pressing Return keeps it
//! (`transcript.editWords`), Escape drops it, and every word the text still has keeps its times.
//!
//! Set `FILMCRAFT_UI_SHOTS=<dir>` to save screenshots of the key steps.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const WORDS: [&str; 7] = ["Hello", "um", "there", "friend.", "This", "is", "great."];

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project with the Text panel's Transcript tab open on seven words of the first A1
    /// clip's media, half a second apart.
    fn with_transcript() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let a = session.active_sequence().unwrap().audio_tracks[0].items[0].clone();
        let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
        let words: Vec<Value> = WORDS
            .iter()
            .enumerate()
            .map(|(i, w)| json!({"text": w, "start": tk(0.5 + i as f64 * 0.5), "end": tk(0.9 + i as f64 * 0.5), "speaker": 0}))
            .collect();
        session.execute("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SHOTS").is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
        d.frames(2);
        d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
        d.frames(3);
        d
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

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(3);
    }

    fn click_word(&mut self, i: usize) {
        self.ok("ui.click", json!({"id": format!("text.transcript.word.{i}")}));
        self.frames(3);
    }

    /// A real double click on a word: two clicks a few frames apart (the control channel's
    /// synthetic input runs one event per frame, which is not how a mouse delivers them), well
    /// inside egui's double-click time.
    fn double_click_word(&mut self, i: usize) {
        let id = format!("text.transcript.word.{i}");
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        let pos = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        // egui counts a click, then two more within 0.6 s of it, as a triple click; the harness's
        // clock only runs 1/60 s a frame, so let the earlier clicks of the test fall out of that
        self.frames(60);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.harness.input_mut().events.push(egui::Event::PointerMoved(pos));
        self.frames(2);
        for _ in 0..2 {
            self.harness.input_mut().events.push(button(true));
            self.frames(1);
            self.harness.input_mut().events.push(button(false));
            self.frames(4);
        }
    }

    /// (text, start, end) of every word of the sequence transcript.
    fn words(&mut self) -> Vec<(String, i64, i64)> {
        let r = self.ok("engine.execute", json!({"command": "transcript.inspect", "params": {}}));
        r["words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (w["text"].as_str().unwrap().to_string(), w["start"].as_i64().unwrap(), w["end"].as_i64().unwrap()))
            .collect()
    }

    fn editing(&mut self) -> bool {
        !self.ids("text.transcript.edit").is_empty()
    }

    /// Replace everything in the open field with `text`.
    fn type_over(&mut self, text: &str) {
        self.key("Cmd+A");
        self.ok("ui.type", json!({"text": text}));
        self.frames(3);
    }

    fn shot(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from) else { return };
        self.frames(3);
        std::fs::create_dir_all(&dir).unwrap();
        self.harness.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
    }
}

fn texts(w: &[(String, i64, i64)]) -> Vec<&str> {
    w.iter().map(|(t, _, _)| t.as_str()).collect()
}

/// Return on a single word opens its whole segment; removing a word and pressing Return keeps the
/// others exactly where they were.
#[test]
fn return_opens_the_segment_and_keeps_every_other_word_in_place() {
    let mut d = Driver::with_transcript();
    let before = d.words();
    assert_eq!(texts(&before), WORDS);
    d.click_word(1);
    assert!(!d.editing());
    d.key("Return");
    assert!(d.editing(), "Return opens the field");
    d.shot("edit-1-open");
    d.type_over("Hello there friend. This is great, really.");
    d.shot("edit-2-typed");
    d.key("Return");
    assert!(!d.editing(), "Return keeps the change and closes the field");
    let after = d.words();
    assert_eq!(texts(&after), ["Hello", "there", "friend.", "This", "is", "great,", "really."]);
    // 'um' is gone; the words that stay the same word are exactly where they were
    for (new_at, old_at) in [(0, 0), (1, 2), (2, 3), (3, 4), (4, 5)] {
        assert_eq!((after[new_at].1, after[new_at].2), (before[old_at].1, before[old_at].2), "{}", after[new_at].0);
    }
    // 'great,' is the same word as 'great.' but the new 'really.' follows it and takes the tail of
    // its time: together they cover what 'great.' covered
    assert_eq!(after[5].1, before[6].1);
    assert_eq!(after[5].2, after[6].1, "no gap between them");
    assert!(after[5].2 > after[5].1 && after[5].2 < before[6].2, "'great,' keeps most of it");
    assert_eq!(after[6].2, before[6].2);
    d.shot("edit-3-saved");
    // one undo step
    d.ok("engine.execute", json!({"command": "edit.undo", "params": {}}));
    d.frames(3);
    assert_eq!(texts(&d.words()), WORDS);
}

/// A selection of several words is exactly what Return edits.
#[test]
fn return_edits_a_multi_word_selection() {
    let mut d = Driver::with_transcript();
    let before = d.words();
    d.click_word(4);
    d.ok("ui.click", json!({"id": "text.transcript.word.6", "modifiers": {"shift": true}}));
    d.frames(3);
    d.key("Return");
    assert!(d.editing());
    d.type_over("That was great.");
    d.key("Return");
    let after = d.words();
    assert_eq!(texts(&after), ["Hello", "um", "there", "friend.", "That", "was", "great."]);
    assert_eq!((after[0].1, after[3].2), (before[0].1, before[3].2), "the words before are untouched");
    assert_eq!(after[6], before[6].clone().into_same_text("great."), "'great.' is still where it was");
}

/// Escape drops what was typed; a double click opens the segment too.
#[test]
fn escape_cancels_and_a_double_click_opens_the_segment() {
    let mut d = Driver::with_transcript();
    let before = d.words();
    d.double_click_word(2);
    assert!(d.editing(), "a double click opens the field");
    d.type_over("something else entirely");
    d.key("Escape");
    assert!(!d.editing(), "Escape closes it");
    assert_eq!(d.words(), before, "and nothing changed");
    // pressing Return without changing the text changes nothing either
    d.click_word(2);
    d.key("Return");
    assert!(d.editing());
    d.key("Return");
    assert!(!d.editing());
    assert_eq!(d.words(), before);
    assert!(!d.ok("engine.execute", json!({"command": "edit.undo", "params": {}}))["undone"].is_null(), "undo still reaches earlier steps");
}

trait SameText {
    fn into_same_text(self, text: &str) -> (String, i64, i64);
}
impl SameText for (String, i64, i64) {
    fn into_same_text(self, text: &str) -> (String, i64, i64) {
        (text.to_string(), self.1, self.2)
    }
}
