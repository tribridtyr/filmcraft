//! Transcripts of media clips (Text panel ▸ Transcript).
//!
//! A [`Transcript`] belongs to one media item (`Project::transcripts`, keyed by the item id) and
//! lists the spoken [`Word`]s with their **media-time** bounds, so it stays valid however the clip
//! is trimmed, moved or reused: the sequence transcript is derived from the clip transcripts by
//! mapping each word through the track items that show it (`filmcraft_edit::transcript`).
//!
//! Speakers are numbered per transcript (`Word::speaker` indexes [`Transcript::speakers`]); the
//! Text panel shows and renames them by name, so two clips whose speakers share a name read as one
//! speaker in the sequence transcript.

use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};

/// One transcribed word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// The word as written, with attached punctuation (`"Hello,"`). No surrounding spaces.
    pub text: String,
    /// Media time of the word's first sample.
    pub start: Tick,
    /// Media time just after the word (exclusive).
    pub end: Tick,
    /// Index into [`Transcript::speakers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// Recogniser confidence 0..1 (1 for hand-made or corrected words).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub confidence: f32,
}

fn one() -> f32 {
    1.0
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}

impl Word {
    pub fn new(text: impl Into<String>, start: Tick, end: Tick) -> Self {
        Self { text: text.into(), start, end, speaker: None, confidence: 1.0 }
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end.max(self.start))
    }
    /// Lower-case text without leading/trailing punctuation (`"Um,"` → `"um"`), for search and
    /// filler-word matching.
    pub fn normalized(&self) -> String {
        normalize_word(&self.text)
    }
}

/// Lower-case `s` and trim punctuation and symbols from both ends (apostrophes inside a word stay:
/// `"Don't!"` → `"don't"`).
pub fn normalize_word(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// A speaker label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    pub name: String,
}

/// The transcript of one media item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    /// ISO 639-1 code of the spoken language (`"en"`).
    pub language: String,
    /// What produced it: a model id (`"whisper-base"`), `"imported"` or `"manual"`.
    pub source: String,
    pub speakers: Vec<Speaker>,
    /// Words in time order, non-overlapping.
    pub words: Vec<Word>,
    /// Where the recording has voice, as `(start, end)` media times measured from the waveform when
    /// it was transcribed (sorted, non-overlapping). The gaps between them are the pauses the Text
    /// panel shows and Delete removes. Empty for transcripts made without the audio (imported, or
    /// from before voice analysis): their pauses are the gaps between words.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub voice: Vec<(Tick, Tick)>,
}

impl Transcript {
    /// The speaker name of `word` (`"Speaker 2"` when the label list is short; `None` when the word
    /// has no speaker).
    pub fn speaker_name(&self, word: &Word) -> Option<String> {
        let i = word.speaker? as usize;
        Some(self.speakers.get(i).map(|s| s.name.clone()).unwrap_or_else(|| format!("Speaker {}", i + 1)))
    }

    /// The text of all words joined by spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// Indices of the words overlapping the media range.
    pub fn words_in(&self, r: TimeRange) -> std::ops::Range<usize> {
        let a = self.words.partition_point(|w| w.end <= r.start);
        let b = self.words.partition_point(|w| w.start < r.end());
        a..b.max(a)
    }

    /// Sort words by time and make the transcript well formed: empty words dropped, `end >= start`,
    /// no overlaps (a word ends at the next one's start at the latest), speaker indices in range
    /// (missing labels are added as "Speaker N").
    pub fn normalize(&mut self) {
        self.words.retain(|w| !w.text.trim().is_empty());
        for w in &mut self.words {
            w.text = w.text.trim().to_string();
            if w.end < w.start {
                w.end = w.start;
            }
            w.confidence = w.confidence.clamp(0.0, 1.0);
        }
        self.words.sort_by_key(|w| (w.start, w.end));
        for i in 1..self.words.len() {
            let s = self.words[i].start;
            if self.words[i - 1].end > s {
                self.words[i - 1].end = s.max(self.words[i - 1].start);
            }
        }
        // voice spans: positive, sorted, overlapping or touching ones merged
        self.voice.retain(|(a, b)| b > a);
        self.voice.sort_unstable();
        let mut merged: Vec<(Tick, Tick)> = Vec::with_capacity(self.voice.len());
        for (a, b) in std::mem::take(&mut self.voice) {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        self.voice = merged;
        let max = self.words.iter().filter_map(|w| w.speaker).max();
        if let Some(m) = max {
            while self.speakers.len() <= m as usize {
                let n = self.speakers.len() + 1;
                self.speakers.push(Speaker { name: format!("Speaker {n}") });
            }
        }
    }

    /// Validate the invariants [`Transcript::normalize`] establishes.
    pub fn check(&self) -> Result<(), String> {
        for (i, w) in self.words.iter().enumerate() {
            if w.end < w.start {
                return Err(format!("word {i} ({:?}) ends before it starts", w.text));
            }
            if i > 0 && self.words[i - 1].end > w.start {
                return Err(format!("word {i} ({:?}) overlaps the word before it", w.text));
            }
            if let Some(s) = w.speaker
                && s as usize >= self.speakers.len()
            {
                return Err(format!("word {i} has speaker {s}, but there are {} speakers", self.speakers.len()));
            }
        }
        for (i, (a, b)) in self.voice.iter().enumerate() {
            if b <= a {
                return Err(format!("voice span {i} is empty or reversed"));
            }
            if i > 0 && self.voice[i - 1].1 >= *a {
                return Err(format!("voice span {i} overlaps or touches the one before it"));
            }
        }
        Ok(())
    }
}

/// The most cells (old words x new words) [`retime_edit`] aligns; a bigger edit is shared out by
/// letters instead, which is slower to be exact about but needs no table.
const ALIGN_CELLS: usize = 4_000_000;

/// Words that stay the same through an edit: pairs `(old index, new index)` of a longest common
/// subsequence of the two key lists (empty keys never match).
fn aligned(old: &[String], new: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (old.len(), new.len());
    if n.saturating_mul(m) > ALIGN_CELLS {
        return Vec::new();
    }
    let w = m + 1;
    // lcs[i * w + j]: longest common subsequence of old[i..] and new[j..]
    let mut lcs = vec![0u32; (n + 1) * w];
    let same = |i: usize, j: usize| matches!((old.get(i), new.get(j)), (Some(a), Some(b)) if !a.is_empty() && a == b);
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let v = if same(i, j) {
                lcs.get((i + 1) * w + j + 1).copied().unwrap_or(0) + 1
            } else {
                lcs.get((i + 1) * w + j).copied().unwrap_or(0).max(lcs.get(i * w + j + 1).copied().unwrap_or(0))
            };
            if let Some(c) = lcs.get_mut(i * w + j) {
                *c = v;
            }
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut pairs = Vec::new();
    while i < n && j < m {
        let here = lcs.get(i * w + j).copied().unwrap_or(0);
        if same(i, j) && here == lcs.get((i + 1) * w + j + 1).copied().unwrap_or(0) + 1 {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if lcs.get((i + 1) * w + j).copied().unwrap_or(0) >= lcs.get(i * w + j + 1).copied().unwrap_or(0) {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Splits the span `(start, end)` into one consecutive piece per text, by letters (a word counts
/// at least one letter). The pieces meet without gaps and the last ends at `end`.
fn share_span(span: (Tick, Tick), texts: &[&str]) -> Vec<(Tick, Tick)> {
    let (start, end) = (span.0.0 as i128, (span.1.0 as i128).max(span.0.0 as i128));
    let weights: Vec<i128> = texts.iter().map(|t| t.chars().count().max(1) as i128).collect();
    let total = weights.iter().sum::<i128>().max(1);
    let tick = |v: i128| Tick(v.clamp(i64::MIN as i128, i64::MAX as i128) as i64);
    let mut at = 0i128;
    let mut out = Vec::with_capacity(texts.len());
    for (k, wt) in weights.iter().enumerate() {
        let a = start + (end - start) * at / total;
        at += wt;
        let b = if k + 1 == weights.len() { end } else { start + (end - start) * at / total };
        out.push((tick(a), tick(b)));
    }
    out
}

/// The words that replace `old` when its text is corrected to `new` (already split into words).
///
/// A word that stays the same word (ignoring case and punctuation) keeps its exact times, so
/// fixing one word of a long segment leaves every other word where it was. A run of changed words
/// takes the times of the old run: one for one when the counts match, otherwise the old span
/// shared out by letters. Words with no old counterpart (an insertion) take their time from the
/// word before, or the word after at the very start. Corrected words have confidence 1; speakers
/// come from the nearest old word. Empty when either side is empty.
pub fn retime_edit(old: &[Word], new: &[&str]) -> Vec<Word> {
    if old.is_empty() || new.is_empty() {
        return Vec::new();
    }
    struct Slot {
        text: String,
        span: Option<(Tick, Tick)>,
        speaker: Option<u32>,
    }
    let keys_old: Vec<String> = old.iter().map(|w| normalize_word(&w.text)).collect();
    let keys_new: Vec<String> = new.iter().map(|w| normalize_word(w)).collect();
    let mut pairs = aligned(&keys_old, &keys_new);
    pairs.push((old.len(), new.len()));
    let mut slots: Vec<Slot> = Vec::with_capacity(new.len());
    let (mut pi, mut pj) = (0usize, 0usize);
    for (i, j) in pairs {
        let run_old = old.get(pi..i).unwrap_or_default();
        let run_new = new.get(pj..j).unwrap_or_default();
        match (run_old.first(), run_old.last()) {
            (Some(f), Some(l)) if !run_new.is_empty() => {
                let spans: Vec<(Tick, Tick)> =
                    if run_old.len() == run_new.len() { run_old.iter().map(|w| (w.start, w.end)).collect() } else { share_span((f.start, l.end), run_new) };
                for (k, (t, span)) in run_new.iter().zip(spans).enumerate() {
                    let speaker = run_old.get(k).map_or(f.speaker, |w| w.speaker);
                    slots.push(Slot { text: (*t).to_string(), span: Some(span), speaker });
                }
            }
            _ => {
                for t in run_new {
                    slots.push(Slot { text: (*t).to_string(), span: None, speaker: None });
                }
            }
        }
        if let (Some(o), Some(t)) = (old.get(i), new.get(j)) {
            slots.push(Slot { text: (*t).to_string(), span: Some((o.start, o.end)), speaker: o.speaker });
        }
        pi = i + 1;
        pj = j + 1;
    }
    // insertions: share the neighbouring word's span between it and the new words
    let mut k = 0;
    while k < slots.len() {
        if slots.get(k).is_some_and(|s| s.span.is_some()) {
            k += 1;
            continue;
        }
        let end = (k..slots.len()).find(|&x| slots.get(x).is_some_and(|s| s.span.is_some())).unwrap_or(slots.len());
        let before = k.checked_sub(1).filter(|&b| slots.get(b).is_some_and(|s| s.span.is_some()));
        let (neighbour, first) = match before {
            Some(b) => (Some(b), b),
            None => (slots.get(end).and_then(|s| s.span).map(|_| end), k),
        };
        let last = if neighbour == Some(end) { end } else { end.saturating_sub(1).max(first) };
        let group: Vec<&str> = slots.get(first..=last.min(slots.len().saturating_sub(1))).unwrap_or_default().iter().map(|s| s.text.as_str()).collect();
        let span = neighbour.and_then(|n| slots.get(n)).and_then(|s| s.span).unwrap_or((Tick::ZERO, Tick::ZERO));
        let speaker = neighbour.and_then(|n| slots.get(n)).and_then(|s| s.speaker);
        let pieces = share_span(span, &group);
        for (x, piece) in (first..).zip(pieces) {
            if let Some(slot) = slots.get_mut(x) {
                slot.span = Some(piece);
                if slot.speaker.is_none() {
                    slot.speaker = speaker;
                }
            }
        }
        k = last.saturating_add(1).max(k + 1);
    }
    slots
        .into_iter()
        .map(|s| {
            let (a, b) = s.span.unwrap_or((Tick::ZERO, Tick::ZERO));
            let mut w = Word::new(s.text, a, b.max(a));
            w.speaker = s.speaker;
            w
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, a: i64, b: i64) -> Word {
        Word::new(t, Tick(a), Tick(b))
    }

    fn times(ws: &[Word]) -> Vec<(i64, i64)> {
        ws.iter().map(|w| (w.start.0, w.end.0)).collect()
    }
    fn texts(ws: &[Word]) -> Vec<&str> {
        ws.iter().map(|w| w.text.as_str()).collect()
    }
    fn well_formed(ws: Vec<Word>) {
        let speakers = vec![Speaker { name: "A".into() }, Speaker { name: "B".into() }];
        let t = Transcript { words: ws, speakers, ..Default::default() };
        t.check().unwrap();
    }

    #[test]
    fn retime_keeps_every_unchanged_word_where_it_was() {
        let old = [w("Hello", 0, 10), w("um", 12, 20), w("world.", 20, 30)];
        // case and punctuation are the same word: the times stay, the text is the new text
        let out = retime_edit(&old, &["hello", "uh", "World!"]);
        assert_eq!(texts(&out), ["hello", "uh", "World!"]);
        assert_eq!(times(&out), [(0, 10), (12, 20), (20, 30)]);
        assert!(out.iter().all(|w| w.confidence == 1.0));
        well_formed(out);
        // deleting a word leaves the others exactly where they were (the gap becomes a pause)
        let out = retime_edit(&old, &["Hello", "world."]);
        assert_eq!(times(&out), [(0, 10), (20, 30)]);
    }

    #[test]
    fn retime_shares_a_changed_run_and_takes_insertions_from_a_neighbour() {
        let mut old = vec![w("one", 0, 10), w("two", 10, 30), w("three", 30, 40)];
        old[0].speaker = Some(0);
        old[1].speaker = Some(1);
        // a changed run with another count: the old span shared out by letters, ends kept
        let out = retime_edit(&old[1..2], &["a", "bb", "ccc"]);
        assert_eq!(out.first().map(|w| w.start.0), Some(10));
        assert_eq!(out.last().map(|w| w.end.0), Some(30));
        assert!(out.windows(2).all(|p| p[0].end == p[1].start));
        assert!(out.iter().all(|w| w.speaker == Some(1)));
        well_formed(out);
        // an insertion in the middle: the word before gives up the tail of its span
        let out = retime_edit(&old, &["one", "two", "extra", "three"]);
        assert_eq!(texts(&out), ["one", "two", "extra", "three"]);
        assert_eq!((times(&out)[0], times(&out)[3]), ((0, 10), (30, 40)), "the other words do not move");
        assert_eq!((out[1].start.0, out[2].end.0), (10, 30), "together they cover what 'two' covered");
        assert_eq!(out[1].end, out[2].start);
        assert_eq!(out[2].speaker, Some(1));
        well_formed(out);
        // an insertion at the very start takes the head of the word after it
        let out = retime_edit(&old, &["well", "one", "two", "three"]);
        assert_eq!(times(&out)[1..], [(times(&out)[1].0, 10), (10, 30), (30, 40)]);
        assert_eq!((out[0].start.0, out[1].end.0), (0, 10));
        assert_eq!(out[0].speaker, Some(0));
        well_formed(out);
    }

    #[test]
    fn retime_survives_empty_and_huge_input() {
        assert!(retime_edit(&[], &["x"]).is_empty());
        assert!(retime_edit(&[w("a", 0, 1)], &[]).is_empty());
        // zero-length and reversed words, and punctuation-only words, never panic
        let odd = [w("--", 5, 5), w("--", 5, 5), w("x", 9, 2)];
        well_formed(retime_edit(&odd, &["--", "y", "--", "--"]));
        // bigger than the alignment table: shared out by letters, still well formed
        let old: Vec<Word> = (0..2100).map(|i| w("a", i * 10, i * 10 + 10)).collect();
        let new_text: Vec<String> = (0..2100).map(|_| "b".into()).collect();
        let new: Vec<&str> = new_text.iter().map(String::as_str).collect();
        let out = retime_edit(&old, &new);
        assert_eq!(out.len(), 2100);
        assert_eq!((out[0].start.0, out[2099].end.0), (0, 21_000));
        well_formed(out);
    }

    #[test]
    fn normalize_sorts_clamps_and_labels() {
        let mut t = Transcript { words: vec![w("b", 10, 30), w(" a ", 0, 15), w("", 40, 50), w("c", 35, 20)], ..Default::default() };
        t.words[0].speaker = Some(1);
        t.normalize();
        assert_eq!(t.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(t.words[0].end, Tick(10));
        assert_eq!(t.words[2].end, Tick(35));
        assert_eq!(t.speakers.len(), 2);
        assert_eq!(t.speaker_name(&t.words[1]).as_deref(), Some("Speaker 2"));
        t.check().unwrap();
    }

    #[test]
    fn voice_spans_are_sorted_merged_and_optional_in_files() {
        let mut t = Transcript { voice: vec![(Tick(50), Tick(60)), (Tick(0), Tick(10)), (Tick(8), Tick(20)), (Tick(30), Tick(30))], ..Default::default() };
        t.normalize();
        assert_eq!(t.voice, vec![(Tick(0), Tick(20)), (Tick(50), Tick(60))]);
        t.check().unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"voice\":[[0,20],[50,60]]"), "{json}");
        // older transcripts have no voice spans and don't write any
        let old: Transcript = serde_json::from_str(r#"{"language":"en","source":"whisper-base","speakers":[],"words":[]}"#).unwrap();
        assert!(old.voice.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("voice"));
        t.voice = vec![(Tick(5), Tick(1))];
        assert!(t.check().is_err());
    }

    #[test]
    fn words_in_range_and_normalized() {
        let t = Transcript { words: vec![w("Um,", 0, 10), w("Don't!", 10, 20), w("go", 25, 30)], ..Default::default() };
        assert_eq!(t.words_in(TimeRange::from_bounds(Tick(5), Tick(24))), 0..2);
        assert_eq!(t.words_in(TimeRange::from_bounds(Tick(20), Tick(25))), 2..2);
        assert_eq!(t.words[0].normalized(), "um");
        assert_eq!(t.words[1].normalized(), "don't");
        assert_eq!(t.text(), "Um, Don't! go");
    }
}
