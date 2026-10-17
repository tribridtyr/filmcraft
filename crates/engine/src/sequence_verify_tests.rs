//! `sequence.verify` is how an agent proves an edit left the sequence sound: every kind of damage
//! it reports has to show up, a healthy sequence has to pass, and nothing may panic on odd input.

use filmcraft_project::Sequence;
use serde_json::{Value, json};

use crate::Session;

/// The demo project with its active sequence changed by `f`, then verified.
fn damaged(f: impl FnOnce(&mut Sequence)) -> (Session, Value) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let id = s.state.active_sequence.unwrap();
    f(std::sync::Arc::make_mut(&mut s.project).sequence_mut(id).unwrap());
    let v = s.execute("sequence.verify", json!({})).unwrap();
    (s, v)
}

fn codes(v: &Value) -> Vec<String> {
    v["issues"].as_array().unwrap().iter().map(|i| i["code"].as_str().unwrap().to_string()).collect()
}

fn issue<'a>(v: &'a Value, code: &str) -> &'a Value {
    v["issues"].as_array().unwrap().iter().find(|i| i["code"] == code).unwrap_or_else(|| panic!("no `{code}` issue in {:?}", codes(v)))
}

#[test]
fn the_demo_sequence_passes() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let v = s.execute("sequence.verify", json!({})).unwrap();
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["errors"], json!(0));
    assert!(v["tracks"].as_array().unwrap().len() >= 2);
    assert!(v["durationSeconds"].as_f64().unwrap() > 0.0);
}

#[test]
fn a_hole_between_clips_is_reported_and_close_gap_fixes_it() {
    let (mut s, v) = damaged(|q| {
        let rate = q.settings.frame_rate;
        for t in q.video_tracks.iter_mut().chain(q.audio_tracks.iter_mut()) {
            t.sync_lock = false;
        }
        // the same hole on picture and sound (sync lock refuses to close a hole on one only)
        for t in [&mut q.video_tracks[0], &mut q.audio_tracks[0]] {
            for i in t.items.iter_mut().skip(1) {
                i.start += rate.tick_of(10);
            }
            for x in &mut t.transitions {
                x.start += rate.tick_of(10);
            }
        }
    });
    let g = issue(&v, "gap");
    assert_eq!(g["track"], json!(q_name(&s, 0)));
    assert!(g["seconds"].as_f64().unwrap() > 0.0 && g["message"].as_str().unwrap().contains("closeGap"));
    // the issue's own advice works
    let at = g["at"].as_i64().unwrap();
    s.execute("sequence.closeGap", json!({"track": "V1", "time": at})).unwrap();
    let v2 = s.execute("sequence.verify", json!({})).unwrap();
    assert!(!v2["issues"].as_array().unwrap().iter().any(|i| i["code"] == "gap" && i["track"] == json!("Video 1")), "{:?}", codes(&v2));
}

fn q_name(s: &Session, i: usize) -> String {
    s.active_sequence().unwrap().video_tracks[i].name.clone()
}

#[test]
fn overlapping_and_empty_clips_are_errors() {
    let (_, v) = damaged(|q| {
        let rate = q.settings.frame_rate;
        let start = q.video_tracks[0].items[0].start;
        q.video_tracks[0].items[1].start = start + rate.tick_of(1);
        q.video_tracks[0].items[2].duration = filmcraft_time::Tick(0);
    });
    assert_eq!(v["ok"], json!(false));
    assert_eq!(issue(&v, "overlap")["severity"], json!("error"));
    assert_eq!(issue(&v, "zero-duration")["severity"], json!("error"));
    assert!(v["errors"].as_u64().unwrap() >= 2);
}

#[test]
fn linked_picture_and_sound_that_drift_apart_are_flagged() {
    let (_, v) = damaged(|q| {
        let rate = q.settings.frame_rate;
        q.video_tracks[0].items[0].link = Some(777);
        let a = &mut q.audio_tracks[0].items[0];
        a.link = Some(777);
        a.start = q.video_tracks[0].items[0].start + rate.tick_of(3);
        a.duration = q.video_tracks[0].items[0].duration;
    });
    let d = issue(&v, "link-start-drift");
    assert_eq!(d["severity"], json!("warning"));
    assert!(d["message"].as_str().unwrap().contains("3 frames"), "{d}");
}

#[test]
fn a_clip_that_plays_past_its_media_is_an_error() {
    let (_, v) = damaged(|q| {
        let c = &mut q.video_tracks[0].items[0];
        c.source_in = filmcraft_time::Tick(c.source_in.0 + 10_000 * filmcraft_time::TICKS_PER_SECOND);
    });
    assert_eq!(issue(&v, "source-overrun")["severity"], json!("error"));
}

#[test]
fn disabled_clips_hidden_tracks_and_unequal_ends_are_warned_about() {
    let (_, v) = damaged(|q| {
        q.video_tracks[0].items[0].enabled = false;
        q.video_tracks[0].enabled = false;
        // sound much shorter than the picture
        q.audio_tracks[0].items.truncate(1);
        q.audio_tracks[0].transitions.clear();
        q.audio_tracks[1].items.clear();
    });
    let c = codes(&v);
    for want in ["clip-disabled", "track-off", "av-length"] {
        assert!(c.iter().any(|x| x == want), "{want} missing in {c:?}");
    }
    let errs: Vec<&Value> = v["issues"].as_array().unwrap().iter().filter(|i| i["severity"] == "error").collect();
    assert_eq!(v["ok"], json!(true), "warnings are not errors: {errs:?}");
}

#[test]
fn an_empty_sequence_and_a_bad_item_are_handled() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Empty", "video": 1, "audio": 1})).unwrap();
    let v = s.execute("sequence.verify", json!({})).unwrap();
    assert_eq!((v["ok"].clone(), v["errors"].clone(), v["durationFrames"].clone()), (json!(true), json!(0), json!(0)));
    assert!(s.execute("sequence.verify", json!({"item": 999_999})).is_err());
    // never panics, and always lists at most the cap while counting everything
    let (_, v) = damaged(|q| {
        for t in &mut q.video_tracks {
            for i in &mut t.items {
                i.duration = filmcraft_time::Tick(i64::MIN);
                i.start = filmcraft_time::Tick(i64::MAX);
            }
        }
    });
    assert_eq!(v["ok"], json!(false));
    assert!(v["listed"].as_u64().unwrap() <= 500);
}
