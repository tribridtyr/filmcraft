//! Decoding Whisper token ids to text from the model's `tokenizer.json` (byte-level BPE: each
//! vocabulary entry is a string of "printable" stand-in characters, one per byte, using the
//! GPT-2 byte↔unicode table; special tokens are listed separately as added tokens).
//!
//! Only decoding is needed (Whisper produces tokens; we never encode text), so the BPE merges are
//! not used.

use std::collections::HashMap;

use crate::SpeechError;

pub struct Tokenizer {
    /// Bytes of each regular token, by id.
    bytes: Vec<Vec<u8>>,
    /// Special (added) tokens by content and by id.
    special: HashMap<String, u32>,
    special_by_id: HashMap<u32, String>,
    pub eot: u32,
    pub sot: u32,
    pub transcribe: u32,
    pub translate: Option<u32>,
    pub no_timestamps: u32,
    pub no_speech: Option<u32>,
    /// `<|0.00|>`; every later id is a timestamp in 0.02 s steps.
    pub timestamp_begin: u32,
    /// Language tokens (`"en"` → id), multilingual models only.
    pub languages: Vec<(String, u32)>,
    /// `<|startofprev|>`: introduces the prompt text (the "previous dialogue").
    pub start_of_prev: Option<u32>,
    /// Regular token (in the byte-to-printable-character spelling of `tokenizer.json`) → id.
    by_str: HashMap<String, u32>,
    /// BPE merge ranks: (left, right) → rank (lower merges first).
    merges: HashMap<(String, String), usize>,
}

/// GPT-2's byte → printable character table, inverted.
fn unicode_to_byte() -> HashMap<char, u8> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    // every code is below 512, so all are valid chars (filter_map rather than a panic path)
    bs.iter().zip(&cs).filter_map(|(&b, &c)| Some((char::from_u32(c)?, b as u8))).collect()
}

/// GPT-2's byte → printable character table.
fn byte_to_unicode() -> HashMap<u8, char> {
    unicode_to_byte().into_iter().map(|(c, b)| (b, c)).collect()
}

/// GPT-2's pre-tokenisation (`'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+`)
/// written out by hand: contractions, words, numbers, punctuation runs (each with an optional
/// leading space) and whitespace runs.
fn pre_tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let class = |c: char| {
        if c.is_alphabetic() {
            1
        } else if c.is_numeric() {
            2
        } else if c.is_whitespace() {
            0
        } else {
            3
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            let rest: String = chars[i + 1..chars.len().min(i + 3)].iter().collect();
            if let Some(n) = ["re", "ve", "ll", "s", "t", "m", "d"].iter().find(|k| rest.starts_with(**k)).map(|k| k.len()) {
                out.push(chars[i..=i + n].iter().collect());
                i += n + 1;
                continue;
            }
        }
        let start = i;
        let lead_space = c == ' ' && chars.get(i + 1).is_some_and(|&n| !n.is_whitespace());
        if class(c) == 0 && !lead_space {
            // a whitespace run; its last space stays for the next piece when a word follows
            let mut j = i;
            while chars.get(j).is_some_and(|&x| x.is_whitespace()) {
                j += 1;
            }
            if j < chars.len() && j - i > 1 {
                j -= 1;
            }
            out.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }
        if lead_space {
            i += 1;
        }
        let k = chars.get(i).map(|&x| class(x)).unwrap_or(0);
        i += 1;
        while chars.get(i).is_some_and(|&x| class(x) == k) {
            i += 1;
        }
        out.push(chars[start..i].iter().collect());
    }
    out
}

impl Tokenizer {
    pub fn from_json(json: &str) -> Result<Self, SpeechError> {
        let bad = |m: &str| SpeechError::Model(format!("tokenizer.json: {m}"));
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| bad(&e.to_string()))?;
        let vocab = v["model"]["vocab"].as_object().ok_or_else(|| bad("no model.vocab"))?;
        let table = unicode_to_byte();
        let max = vocab.values().filter_map(|x| x.as_u64()).max().unwrap_or(0) as usize;
        let mut bytes = vec![Vec::new(); max + 1];
        let mut by_str = HashMap::new();
        for (tok, id) in vocab {
            let id = id.as_u64().ok_or_else(|| bad("vocab id"))? as usize;
            if let Some(slot) = bytes.get_mut(id) {
                *slot = tok.chars().filter_map(|c| table.get(&c).copied()).collect();
            }
            by_str.insert(tok.clone(), id as u32);
        }
        // merges are "a b" strings (or [a, b] pairs in newer tokenizer.json files), best first
        let mut merges = HashMap::new();
        for (rank, m) in v["model"]["merges"].as_array().map(Vec::as_slice).unwrap_or(&[]).iter().enumerate() {
            let pair = match (m.as_str(), m.as_array()) {
                (Some(s), _) => s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())),
                (_, Some(a)) => a.first().and_then(|x| x.as_str()).zip(a.get(1).and_then(|x| x.as_str())).map(|(a, b)| (a.to_string(), b.to_string())),
                _ => None,
            };
            if let Some(pair) = pair {
                merges.entry(pair).or_insert(rank);
            }
        }
        let mut special = HashMap::new();
        let mut special_by_id = HashMap::new();
        for a in v["added_tokens"].as_array().ok_or_else(|| bad("no added_tokens"))? {
            let (Some(id), Some(c)) = (a["id"].as_u64(), a["content"].as_str()) else { continue };
            special.insert(c.to_string(), id as u32);
            special_by_id.insert(id as u32, c.to_string());
        }
        let get = |k: &str| special.get(k).copied();
        let need = |k: &str| get(k).ok_or_else(|| bad(&format!("no {k}")));
        let sot = need("<|startoftranscript|>")?;
        let translate = get("<|translate|>");
        let mut languages: Vec<(String, u32)> = special
            .iter()
            .filter(|(k, id)| **id > sot && translate.is_none_or(|t| **id < t) && k.starts_with("<|") && k.ends_with("|>"))
            .map(|(k, id)| (k[2..k.len() - 2].to_string(), *id))
            .collect();
        languages.sort_by_key(|x| x.1);
        Ok(Self {
            eot: need("<|endoftext|>")?,
            sot,
            transcribe: need("<|transcribe|>")?,
            translate,
            no_timestamps: need("<|notimestamps|>")?,
            no_speech: get("<|nospeech|>").or_else(|| get("<|nocaptions|>")),
            timestamp_begin: need("<|0.00|>")?,
            start_of_prev: get("<|startofprev|>"),
            by_str,
            merges,
            languages,
            bytes,
            special,
            special_by_id,
        })
    }

    pub fn multilingual(&self) -> bool {
        !self.languages.is_empty()
    }

    pub fn language_token(&self, code: &str) -> Option<u32> {
        self.languages.iter().find(|(c, _)| c == code).map(|x| x.1)
    }

    pub fn is_timestamp(&self, id: u32) -> bool {
        id >= self.timestamp_begin
    }

    /// The token ids that introduce `prompt` as previous dialogue: `<|startofprev|>` followed by
    /// the text (with a leading space, as Whisper's own prompting does), at most `max` tokens
    /// long (the end is kept). Empty when the model has no such token or the text is empty or
    /// cannot be spelled with the vocabulary.
    pub fn prompt_tokens(&self, prompt: &str, max: usize) -> Vec<u32> {
        let (Some(sop), true) = (self.start_of_prev, max > 1) else { return Vec::new() };
        if prompt.trim().is_empty() {
            return Vec::new();
        }
        let text = self.encode(&format!(" {}", prompt.trim()));
        if text.is_empty() {
            return Vec::new();
        }
        let keep = max - 1;
        let from = text.len().saturating_sub(keep);
        let mut out = vec![sop];
        out.extend_from_slice(text.get(from..).unwrap_or(&[]));
        out
    }

    /// Byte-level BPE encoding of `text` (GPT-2 style pre-tokenisation). Pieces the vocabulary
    /// cannot spell are skipped. Only used for prompts, so it favours clarity over speed.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let to_char = byte_to_unicode();
        let mut out = Vec::new();
        for piece in pre_tokenize(text) {
            let mut parts: Vec<String> = piece.bytes().filter_map(|b| to_char.get(&b).map(|c| c.to_string())).collect();
            // repeatedly merge the adjacent pair with the lowest rank
            while parts.len() > 1 {
                let best = (0..parts.len() - 1)
                    .filter_map(|i| Some((i, *self.merges.get(&(parts.get(i)?.clone(), parts.get(i + 1)?.clone()))?)))
                    .min_by_key(|&(_, r)| r);
                let Some((i, _)) = best else { break };
                let right = parts.remove(i + 1);
                if let Some(left) = parts.get_mut(i) {
                    left.push_str(&right);
                }
            }
            out.extend(parts.iter().filter_map(|p| self.by_str.get(p).copied()));
        }
        out
    }

    /// Bytes of a regular token (empty for special tokens).
    pub fn token_bytes(&self, id: u32) -> &[u8] {
        if self.special_by_id.contains_key(&id) {
            return &[];
        }
        self.bytes.get(id as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        let b: Vec<u8> = ids.iter().flat_map(|&i| self.token_bytes(i).iter().copied()).collect();
        String::from_utf8_lossy(&b).into_owned()
    }

    /// Ids of all special tokens that must never be sampled as text (everything special except
    /// end-of-text and the timestamps).
    pub fn non_text_specials(&self) -> Vec<u32> {
        self.special.values().copied().filter(|&id| id != self.eot && id < self.timestamp_begin).collect()
    }
}

/// Group text tokens into words: a token whose text starts with a space starts a new word;
/// punctuation-only tokens join the word before them (opening quotes/brackets the word after).
/// Returns (word text, token index range).
pub fn group_words(tok: &Tokenizer, ids: &[u32]) -> Vec<(String, std::ops::Range<usize>)> {
    let mut groups: Vec<(Vec<u8>, std::ops::Range<usize>)> = Vec::new();
    let mut pending_open: Option<(Vec<u8>, usize)> = None;
    for (i, &id) in ids.iter().enumerate() {
        let b = tok.token_bytes(id);
        if b.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(b);
        let trimmed = text.trim();
        let starts_space = b[0] == b' ';
        let punct = !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_punctuation() || "“”‘’«»¿¡…。，！？：、".contains(c));
        let opening = punct && trimmed.chars().all(|c| "\"'“‘¿¡([{«-".contains(c)) && starts_space;
        if opening {
            let start = pending_open.as_ref().map(|p| p.1).unwrap_or(i);
            let mut acc = pending_open.take().map(|p| p.0).unwrap_or_default();
            acc.extend_from_slice(b);
            pending_open = Some((acc, start));
            continue;
        }
        if let Some((mut pre, start)) = pending_open.take() {
            pre.extend_from_slice(b);
            groups.push((pre, start..i + 1));
            continue;
        }
        match groups.last_mut() {
            Some(g) if !starts_space => {
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            Some(g) if punct => {
                // " ," style punctuation with a space still belongs to the previous word
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            _ => groups.push((b.to_vec(), i..i + 1)),
        }
    }
    if let Some((pre, start)) = pending_open {
        groups.push((pre, start..ids.len()));
    }
    groups.into_iter().map(|(b, r)| (String::from_utf8_lossy(&b).trim().to_string(), r)).filter(|(t, _)| !t.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny byte-level BPE vocabulary: single bytes, a few merges, the specials.
    fn tiny() -> Tokenizer {
        let to = byte_to_unicode();
        let sp = to[&b' '].to_string();
        let mut vocab = serde_json::Map::new();
        let mut id = 0u64;
        let mut add = |k: String| {
            vocab.insert(k, serde_json::json!(id));
            id += 1;
        };
        for b in 0..=255u8 {
            add(to[&b].to_string());
        }
        // merges: "u"+"m" -> "um", " "+"um" -> " um", "h"+"m" -> "hm"
        add("um".into());
        add(format!("{sp}um"));
        add("hm".into());
        let merges = vec![format!("u m"), format!("{sp} um"), "h m".to_string()];
        let mut added = Vec::new();
        for (i, c) in [
            "<|endoftext|>",
            "<|startoftranscript|>",
            "<|en|>",
            "<|translate|>",
            "<|transcribe|>",
            "<|startofprev|>",
            "<|nospeech|>",
            "<|notimestamps|>",
            "<|0.00|>",
        ]
        .iter()
        .enumerate()
        {
            added.push(serde_json::json!({"id": 1000 + i, "content": c}));
        }
        let json = serde_json::json!({"model": {"vocab": vocab, "merges": merges}, "added_tokens": added});
        Tokenizer::from_json(&json.to_string()).unwrap()
    }

    #[test]
    fn encodes_with_merges_and_round_trips() {
        let t = tiny();
        let ids = t.encode(" um, hm");
        // " um" is one merged token; "," and " " + "hm" follow
        assert_eq!(ids.len(), 4, "{ids:?}");
        assert_eq!(t.decode(&ids), " um, hm");
        assert_eq!(t.decode(&t.encode("Umm, here's 2 things...")), "Umm, here's 2 things...");
    }

    #[test]
    fn prompt_has_start_of_prev_and_is_capped() {
        let t = tiny();
        let p = t.prompt_tokens("um um um um um", 4);
        assert_eq!(p.first().copied(), t.start_of_prev);
        assert_eq!(p.len(), 4);
        assert!(t.prompt_tokens("", 8).is_empty());
        assert!(t.prompt_tokens("um", 1).is_empty());
    }

    #[test]
    fn pre_tokenize_matches_gpt2_rules() {
        assert_eq!(pre_tokenize("Umm, let me"), ["Umm", ",", " let", " me"]);
        assert_eq!(pre_tokenize("here's what"), ["here", "'s", " what"]);
        assert_eq!(pre_tokenize("a  b"), ["a", " ", " b"]);
        assert_eq!(pre_tokenize("hmm..."), ["hmm", "..."]);
        assert_eq!(pre_tokenize("it's 2 am"), ["it", "'s", " 2", " am"]);
    }

    #[test]
    fn hostile_text_does_not_panic() {
        let t = tiny();
        for s in ["", "'", "''s", "\u{0}\u{1F600}", "   ", "\n\n x", "'re'"] {
            let _ = t.encode(s);
            let _ = t.prompt_tokens(s, 3);
        }
    }
}
