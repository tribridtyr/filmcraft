//! `sequence.verify`: the check an agent runs after every edit. It reads the active sequence (it
//! changes nothing) and lists what is structurally wrong: clips that overlap, holes between clips,
//! picture and sound of one linked clip that have drifted apart, clips that play past the end of
//! their media, clips on media marked offline, disabled clips and tracks, transitions whose clips are
//! gone, and tracks that end at different times. Each issue says where (track, clips, time) so the
//! agent can fix it with a command; `ok` is true when there is no `error`.

use std::collections::BTreeMap;

use filmcraft_project::{Sequence, Track, TrackItem};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::Session;

/// The most issues listed (the counts stay exact): a broken sequence must not produce a huge reply.
const MAX_ISSUES: usize = 500;

#[derive(Default)]
struct Issues {
    list: Vec<Value>,
    errors: usize,
    warnings: usize,
    infos: usize,
    rate: Option<FrameRate>,
}

impl Issues {
    fn add(&mut self, severity: &str, code: &str, track: &str, clips: &[u64], at: Tick, message: String) {
        match severity {
            "error" => self.errors = self.errors.saturating_add(1),
            "warning" => self.warnings = self.warnings.saturating_add(1),
            _ => self.infos = self.infos.saturating_add(1),
        }
        if self.list.len() >= MAX_ISSUES {
            return;
        }
        let frame = self.rate.map(|r| r.frame_at(at));
        self.list.push(json!({
            "severity": severity, "code": code, "track": track, "clips": clips,
            "at": at.0, "frame": frame, "seconds": seconds(at), "message": message,
        }));
    }
}

fn seconds(t: Tick) -> f64 {
    t.0 as f64 / TICKS_PER_SECOND as f64
}

fn frames_text(rate: FrameRate, a: Tick, b: Tick) -> String {
    format!("{} frames", rate.frame_at(Tick(a.0.saturating_sub(b.0))).abs())
}

/// All the checks, on one sequence.
pub fn verify(s: &Session, q: &Sequence) -> Value {
    let rate = q.settings.frame_rate;
    let one_frame = rate.tick_of(1);
    let mut out = Issues { rate: Some(rate), ..Default::default() };
    let mut clip_ids: BTreeMap<u64, (&str, &TrackItem)> = BTreeMap::new();
    let mut summaries = Vec::new();
    let mut ends: Vec<(String, Tick)> = Vec::new();

    for (kind, tracks) in [("video", &q.video_tracks), ("audio", &q.audio_tracks)] {
        for t in tracks {
            for i in &t.items {
                clip_ids.insert(i.id.0, (kind, i));
            }
            let (gaps, end) = check_track(s, &mut out, t, kind, one_frame);
            summaries.push(json!({"track": t.name, "kind": kind, "clips": t.items.len(), "gaps": gaps, "end": end.0, "endSeconds": seconds(end)}));
            if !t.items.is_empty() {
                ends.push((t.name.clone(), end));
            }
        }
    }

    check_links(&mut out, q, rate);
    check_transitions(&mut out, q, &clip_ids);

    // tracks that end at different times (picture shorter than sound, or the reverse)
    let video_end = ends.iter().filter(|(n, _)| n.starts_with('V')).map(|(_, e)| *e).max();
    let audio_end = ends.iter().filter(|(n, _)| n.starts_with('A')).map(|(_, e)| *e).max();
    if let (Some(v), Some(a)) = (video_end, audio_end)
        && (v.0.saturating_sub(a.0)).abs() > one_frame.0
    {
        let (what, at) = if v > a { ("sound ends before the picture", a) } else { ("picture ends before the sound", v) };
        out.add("warning", "av-length", "", &[], at, format!("{what} (video ends at {:.3}s, audio at {:.3}s)", seconds(v), seconds(a)));
    }

    let duration = q.duration();
    json!({
        "ok": out.errors == 0,
        "errors": out.errors,
        "warnings": out.warnings,
        "infos": out.infos,
        "listed": out.list.len(),
        "duration": duration.0,
        "durationSeconds": seconds(duration),
        "durationFrames": rate.frame_at(duration),
        "tracks": summaries,
        "issues": out.list,
    })
}

/// Overlaps, holes, empty clips and media problems on one track. Returns (holes between clips, end).
fn check_track(s: &Session, out: &mut Issues, t: &Track, kind: &str, one_frame: Tick) -> (usize, Tick) {
    let mut items: Vec<&TrackItem> = t.items.iter().collect();
    items.sort_by_key(|i| (i.start, i.id.0));
    let mut gaps = 0usize;
    let mut end = Tick(0);
    let mut prev: Option<&TrackItem> = None;

    if !t.items.is_empty() {
        if kind == "video" && !t.enabled {
            out.add("warning", "track-off", &t.name, &[], Tick(0), format!("track {} is hidden (its eye is off): its clips do not show", t.name));
        }
        if kind == "audio" && (t.muted || !t.enabled) {
            out.add("info", "track-muted", &t.name, &[], Tick(0), format!("track {} is muted: its clips are silent", t.name));
        }
    }

    for i in items {
        let id = i.id.0;
        if i.duration.0 <= 0 {
            out.add("error", "zero-duration", &t.name, &[id], i.start, format!("clip {id} ({}) has no length", i.name));
        }
        if i.start.0 < 0 {
            out.add("error", "negative-start", &t.name, &[id], i.start, format!("clip {id} ({}) starts before the sequence", i.name));
        }
        if let Some(p) = prev {
            if i.start < p.end() {
                let by = Tick(p.end().0.saturating_sub(i.start.0));
                out.add("error", "overlap", &t.name, &[p.id.0, id], i.start, format!("clip {id} starts {:.3}s before clip {} ends", seconds(by), p.id.0));
            } else if i.start > p.end() {
                gaps = gaps.saturating_add(1);
                let hole = Tick(i.start.0.saturating_sub(p.end().0));
                out.add(
                    "warning",
                    "gap",
                    &t.name,
                    &[p.id.0, id],
                    p.end(),
                    format!("{:.3}s gap between clip {} and clip {id}: close it with sequence.closeGap {{\"track\":\"{}\",\"time\":{}}} (refused with a sync-lock conflict when another synced track has no gap there)", seconds(hole), p.id.0, t.name, p.end().0),
                );
            }
        } else if i.start.0 > 0 {
            out.add("info", "leading-gap", &t.name, &[id], Tick(0), format!("track {} is empty for the first {:.3}s", t.name, seconds(i.start)));
        }
        if !i.enabled {
            out.add("warning", "clip-disabled", &t.name, &[id], i.start, format!("clip {id} ({}) is disabled and will not play", i.name));
        }
        check_media(s, out, t, i, one_frame);
        if i.end() > end {
            end = i.end();
        }
        prev = Some(i);
    }
    (gaps, end)
}

/// A clip must play inside its media, and the media must be there.
fn check_media(s: &Session, out: &mut Issues, t: &Track, i: &TrackItem, one_frame: Tick) {
    let Some(item) = s.project.item(i.item) else {
        out.add("error", "missing-item", &t.name, &[i.id.0], i.start, format!("clip {} points at a project item ({}) that does not exist", i.id.0, i.item.0));
        return;
    };
    if i.source_in.0 < 0 {
        out.add(
            "error",
            "source-before-start",
            &t.name,
            &[i.id.0],
            i.start,
            format!("clip {} starts {:.3}s before its media begins", i.id.0, seconds(Tick(i.source_in.0.saturating_neg()))),
        );
    }
    let Some(m) = item.as_media() else { return };
    if m.offline {
        out.add("warning", "offline-media", &t.name, &[i.id.0], i.start, format!("clip {} uses offline media ({})", i.id.0, item.name));
    }
    // stills and generators have no real length; only footage can be overrun
    let len = m.duration();
    if len.0 > 0 && i.frame_hold.is_none() && i.source_out().0.saturating_sub(len.0) > one_frame.0 {
        let over = Tick(i.source_out().0.saturating_sub(len.0));
        out.add(
            "error",
            "source-overrun",
            &t.name,
            &[i.id.0],
            i.start,
            format!("clip {} plays {:.3}s past the end of its media ({}, {:.3}s long)", i.id.0, seconds(over), item.name, seconds(len)),
        );
    }
}

/// Clips linked to each other (a video and its sound) should start and last the same.
fn check_links(out: &mut Issues, q: &Sequence, rate: FrameRate) {
    let mut groups: BTreeMap<u64, Vec<(&str, &TrackItem)>> = BTreeMap::new();
    for (name, tracks) in [("video", &q.video_tracks), ("audio", &q.audio_tracks)] {
        for t in tracks {
            for i in &t.items {
                if let Some(l) = i.link {
                    groups.entry(l).or_default().push((name, i));
                }
            }
        }
    }
    for members in groups.values() {
        let Some((_, first)) = members.first().copied() else { continue };
        if members.len() == 1 {
            continue;
        }
        for (_, other) in members.iter().skip(1) {
            let ids = [first.id.0, other.id.0];
            let off = Tick(other.start.0.saturating_sub(first.start.0));
            if off.0 != 0 {
                out.add(
                    "warning",
                    "link-start-drift",
                    "",
                    &ids,
                    first.start.min(other.start),
                    format!(
                        "linked clips {} and {} start {} apart (unlink them first if that is a J or L cut)",
                        ids[0],
                        ids[1],
                        frames_text(rate, other.start, first.start)
                    ),
                );
            }
            if other.duration != first.duration {
                out.add(
                    "warning",
                    "link-length-drift",
                    "",
                    &ids,
                    first.start.min(other.start),
                    format!("linked clips {} and {} differ in length by {}", ids[0], ids[1], frames_text(rate, other.duration, first.duration)),
                );
            }
        }
    }
}

fn check_transitions(out: &mut Issues, q: &Sequence, clips: &BTreeMap<u64, (&str, &TrackItem)>) {
    for t in q.video_tracks.iter().chain(q.audio_tracks.iter()) {
        for x in &t.transitions {
            for (side, c) in [("outgoing", x.from), ("incoming", x.to)] {
                if let Some(c) = c
                    && !clips.contains_key(&c.0)
                {
                    out.add(
                        "error",
                        "transition-orphan",
                        &t.name,
                        &[c.0],
                        x.start,
                        format!("a transition on {} names an {side} clip ({}) that is not in the sequence", t.name, c.0),
                    );
                }
            }
            if x.duration.0 <= 0 {
                out.add("error", "transition-empty", &t.name, &[], x.start, format!("a transition on {} has no length", t.name));
            }
        }
    }
}
