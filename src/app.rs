use crate::sentence::{Config, Sentence, Settings};
use rand::rng;
use rand::seq::SliceRandom;
use std::process::{Child, Command};

// Parse a whitespace-separated tag string into clean tags (a leading '#' is
// optional, and empty entries are dropped). Free function so it can be tested.
fn parse_tags(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(|t| t.trim_start_matches('#').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

// Which mode the app is in. In Normal we navigate; in Search we type a filter;
// in Add we type a sentence (first its text, then its tags) — also reused for
// editing; in ConfirmDelete we ask before removing the selected sentence.
#[derive(PartialEq)] // lets us compare modes with ==
pub enum Mode {
    Normal,
    Search,
    Add,
    ConfirmDelete,
    Help,
    Settings,
}

pub struct App {
    pub sentences: Vec<Sentence>,
    pub selected: usize,
    pub filter: String,
    pub mode: Mode,
    pub playing: bool,             // auto-play on/off
    pub playlist: Vec<usize>,      // shuffled order of sentence indices
    pub play_pos: usize,           // how far through the playlist
    pub pause_secs: u64,           // seconds between sentences
    pub settings: Settings,        // speech settings (voice/rate) for `say`
    say_child: Option<Child>,      // the currently running `say` process, if any
    speaking_index: Option<usize>, // real index of the sentence currently being spoken
    pub add_text: String,          // buffer: text of the sentence being added
    pub add_tags: String,          // buffer: tags of the sentence being added
    pub add_note: String,          // buffer: note/comment of the sentence being added
    pub add_field: usize,          // active Add field: 0 phrase, 1 tags, 2 note
    pub editing: Option<usize>,    // Add mode: Some(real index) when editing, None when adding
    pub set_voice: String,         // Settings buffer: voice
    pub set_rate: String,          // Settings buffer: rate (digits)
    pub set_star_weight: String,   // Settings buffer: star weight (digits)
    pub set_field: usize,          // active Settings field: 0 voice, 1 rate, 2 star weight
}

impl App {
    // Build a new shuffled playlist. Starred ("difficult") sentences are
    // weighted: they appear `star_weight` times in the pool, so auto-play
    // surfaces them more often. The weight comes from settings (default 3),
    // clamped to at least 1 so starred sentences are never dropped.
    pub fn reshuffle(&mut self) {
        let star_weight = self.settings.star_weight.unwrap_or(3).max(1) as usize;
        // Each index appears once, or `star_weight` times if starred.
        self.playlist = self
            .sentences
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                let weight = if s.starred { star_weight } else { 1 };
                std::iter::repeat_n(i, weight)
            })
            .collect();
        // Shuffle them in place using a random number generator.
        self.playlist.shuffle(&mut rng());
        // Back to the start of the new playlist.
        self.play_pos = 0;
    }

    pub fn new(config: Config) -> Self {
        App {
            sentences: config.sentences,
            selected: 0,
            filter: String::new(),
            mode: Mode::Normal,
            playing: false,
            playlist: Vec::new(), // built when play starts
            play_pos: 0,
            pause_secs: 4, // default 4-second gap
            settings: config.settings,
            say_child: None, // nothing speaking yet
            speaking_index: None,
            add_text: String::new(),
            add_tags: String::new(),
            add_note: String::new(),
            add_field: 0,
            editing: None,
            set_voice: String::new(),
            set_rate: String::new(),
            set_star_weight: String::new(),
            set_field: 0,
        }
    }

    // Real index (into `sentences`) of the currently selected visible row.
    fn selected_real_index(&self) -> Option<usize> {
        self.matches().get(self.selected).copied()
    }

    // Text of the currently selected visible sentence (for the delete prompt).
    pub fn selected_text(&self) -> Option<String> {
        self.selected_sentence().map(|s| s.text.clone())
    }

    // The currently selected visible sentence (for the detail panel).
    pub fn selected_sentence(&self) -> Option<&Sentence> {
        self.selected_real_index()
            .and_then(|i| self.sentences.get(i))
    }

    // macOS `say` defaults to ~175 wpm, so an unset rate is shown/adjusted
    // from there. Speed is stepped by 10 wpm and clamped to a sane range so
    // speech stays intelligible.
    const DEFAULT_RATE: u32 = 175;

    // The effective speaking rate (words/min) for display: the configured one,
    // or the macOS default when unset.
    pub fn rate_wpm(&self) -> u32 {
        self.settings.rate.unwrap_or(Self::DEFAULT_RATE)
    }

    // Bump the speaking rate by `delta` steps of 10 wpm (negative = slower),
    // clamp it to a usable range, and persist it like the Settings screen does.
    pub fn adjust_rate(&mut self, delta: i32) {
        const STEP: i32 = 10;
        const MIN: i32 = 80;
        const MAX: i32 = 400;
        let next = (self.rate_wpm() as i32 + delta * STEP).clamp(MIN, MAX) as u32;
        self.settings.rate = Some(next);
        let _ = crate::sentence::save_all(&self.settings, &self.sentences);
    }

    // Toggle the "starred" flag on the selected sentence and persist it.
    pub fn toggle_star(&mut self) {
        let Some(real) = self.selected_real_index() else {
            return;
        };
        let Some(s) = self.sentences.get_mut(real) else {
            return;
        };
        s.starred = !s.starred;
        let _ = crate::sentence::save_all(&self.settings, &self.sentences);
    }

    // --- Settings mode: edit voice / rate / star weight ---

    // The buffer for the active Settings field.
    fn settings_buffer_mut(&mut self) -> &mut String {
        match self.set_field {
            0 => &mut self.set_voice,
            1 => &mut self.set_rate,
            _ => &mut self.set_star_weight,
        }
    }

    // Enter Settings mode, pre-filling the buffers from the current settings.
    pub fn start_settings(&mut self) {
        self.set_voice = self.settings.voice.clone().unwrap_or_default();
        self.set_rate = self
            .settings
            .rate
            .map(|r| r.to_string())
            .unwrap_or_default();
        self.set_star_weight = self
            .settings
            .star_weight
            .map(|w| w.to_string())
            .unwrap_or_default();
        self.set_field = 0;
        self.mode = Mode::Settings;
    }

    // Leave Settings mode without saving.
    pub fn cancel_settings(&mut self) {
        self.mode = Mode::Normal;
    }

    // Type into the active Settings field. The numeric fields accept digits only.
    pub fn settings_char(&mut self, c: char) {
        if self.set_field != 0 && !c.is_ascii_digit() {
            return;
        }
        self.settings_buffer_mut().push(c);
    }

    // Backspace in the active Settings field.
    pub fn settings_backspace(&mut self) {
        self.settings_buffer_mut().pop();
    }

    // Enter in Settings mode: advance voice -> rate -> star weight, then save.
    pub fn settings_enter(&mut self) {
        if self.set_field < 2 {
            self.set_field += 1;
        } else {
            self.commit_settings();
        }
    }

    // Apply the buffered settings (empty field = unset) and persist them.
    fn commit_settings(&mut self) {
        let voice = self.set_voice.trim();
        self.settings.voice = if voice.is_empty() {
            None
        } else {
            Some(voice.to_string())
        };
        // Digit-only buffers: a valid parse means set, empty means unset.
        self.settings.rate = self.set_rate.trim().parse().ok();
        self.settings.star_weight = self.set_star_weight.trim().parse().ok();
        let _ = crate::sentence::save_all(&self.settings, &self.sentences);
        self.mode = Mode::Normal;
    }

    // Real index of the sentence still being spoken, or None. Reaps the `say`
    // process once it has finished so the “speaking” marker clears itself.
    pub fn poll_speaking(&mut self) -> Option<usize> {
        match self.say_child.as_mut() {
            Some(child) => {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    self.say_child = None;
                    self.speaking_index = None;
                }
            }
            None => self.speaking_index = None,
        }
        self.speaking_index
    }

    // Keep `selected` within the current match list (e.g. after a delete).
    fn clamp_selected(&mut self) {
        let count = self.matches().len();
        if self.selected >= count {
            self.selected = count.saturating_sub(1);
        }
    }

    // --- Add / Edit mode: capture a sentence (text, then tags, then note) ---

    // The buffer for the field currently being typed into.
    fn add_buffer_mut(&mut self) -> &mut String {
        match self.add_field {
            0 => &mut self.add_text,
            1 => &mut self.add_tags,
            _ => &mut self.add_note,
        }
    }

    // Enter Add mode for a brand-new sentence, with empty buffers.
    pub fn start_add(&mut self) {
        self.mode = Mode::Add;
        self.add_field = 0;
        self.editing = None;
        self.add_text.clear();
        self.add_tags.clear();
        self.add_note.clear();
    }

    // Enter Add mode to edit the selected sentence, pre-filling its fields.
    pub fn start_edit(&mut self) {
        if let Some(real) = self.selected_real_index()
            && let Some(s) = self.sentences.get(real)
        {
            self.add_text = s.text.clone();
            self.add_tags = s.tags.join(" ");
            self.add_note = s.note.clone();
            self.editing = Some(real);
            self.add_field = 0;
            self.mode = Mode::Add;
        }
    }

    // Leave Add mode without saving.
    pub fn cancel_add(&mut self) {
        self.mode = Mode::Normal;
        self.add_text.clear();
        self.add_tags.clear();
        self.add_note.clear();
        self.add_field = 0;
        self.editing = None;
    }

    // Type into whichever Add field is active.
    pub fn add_char(&mut self, c: char) {
        self.add_buffer_mut().push(c);
    }

    // Backspace in whichever Add field is active.
    pub fn add_backspace(&mut self) {
        self.add_buffer_mut().pop();
    }

    // Enter in Add mode: advance phrase -> tags -> note, then commit.
    pub fn add_enter(&mut self) {
        match self.add_field {
            // Don't advance off an empty phrase (the only required field).
            0 if self.add_text.trim().is_empty() => {}
            0 | 1 => self.add_field += 1,
            _ => self.commit_add(),
        }
    }

    // Save the buffered sentence. When editing, replace the existing one and
    // rewrite the whole file; when adding, append a new block.
    fn commit_add(&mut self) {
        let text = self.add_text.trim().to_string();
        if text.is_empty() {
            self.cancel_add();
            return;
        }
        let tags = parse_tags(&self.add_tags);
        let note = self.add_note.trim().to_string();

        match self.editing {
            Some(real) if real < self.sentences.len() => {
                self.sentences[real].text = text;
                self.sentences[real].tags = tags;
                self.sentences[real].note = note;
                let _ = crate::sentence::save_all(&self.settings, &self.sentences);
            }
            _ => {
                // Persist first; only keep it in memory if the write succeeded.
                if crate::sentence::append(&text, &tags, &note).is_ok() {
                    self.sentences.push(Sentence {
                        text,
                        tags,
                        note,
                        starred: false,
                    });
                }
            }
        }
        self.clamp_selected();
        self.cancel_add();
    }

    // --- Delete mode: confirm, then remove the selected sentence ---

    // Ask for confirmation (only if something is actually selected).
    pub fn start_delete(&mut self) {
        if self.selected_real_index().is_some() {
            self.mode = Mode::ConfirmDelete;
        }
    }

    // Back out of the confirmation without deleting.
    pub fn cancel_delete(&mut self) {
        self.mode = Mode::Normal;
    }

    // Remove the selected sentence and rewrite the file.
    pub fn confirm_delete(&mut self) {
        if let Some(real) = self.selected_real_index() {
            self.sentences.remove(real);
            let _ = crate::sentence::save_all(&self.settings, &self.sentences);
            self.clamp_selected();
        }
        self.mode = Mode::Normal;
    }

    // Stop any sentence currently being spoken (and reap the process).
    pub fn stop(&mut self) {
        if let Some(mut child) = self.say_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.speaking_index = None;
    }

    // Speak a sentence with macOS `say`. Any sentence still being spoken is
    // stopped first, so voices never overlap (and the old child is reaped).
    // Applies the configured voice (`-v`) and rate (`-r`) when set.
    //
    // A trailing `[[slnc N]]` (an embedded `say` command for N ms of silence)
    // pads the end so macOS' tendency to clip the last sample falls on the
    // silence instead of the final word.
    fn say_text(&mut self, text: &str) {
        const TAIL_SILENCE_MS: u32 = 700;
        self.stop();
        let mut cmd = Command::new("say");
        if let Some(voice) = &self.settings.voice {
            cmd.arg("-v").arg(voice);
        }
        if let Some(rate) = self.settings.rate {
            cmd.arg("-r").arg(rate.to_string());
        }
        cmd.arg(format!("{text} [[slnc {TAIL_SILENCE_MS}]]"));
        self.say_child = cmd.spawn().ok();
    }

    // Return the indices of sentences that match the current filter.
    // An empty filter matches everything.
    // A non-empty filter matches if the text OR any tag contains it.
    pub fn matches(&self) -> Vec<usize> {
        // Lowercase the filter once, so search is case-insensitive.
        let needle = self.filter.to_lowercase();

        self.sentences
            .iter()
            .enumerate() // gives us (index, sentence) pairs
            .filter(|(_i, s)| {
                // Empty filter: keep everything.
                if needle.is_empty() {
                    return true;
                }
                // Does the sentence text contain the needle?
                let in_text = s.text.to_lowercase().contains(&needle);
                // Does any tag contain the needle?
                let in_tags = s
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle));
                in_text || in_tags
            })
            .map(|(i, _s)| i) // keep only the index, drop the sentence
            .collect()
    }
    // Speak the next sentence in the playlist, advancing position.
    // Reshuffles when the playlist is exhausted (or empty).
    pub fn advance(&mut self) {
        if self.playlist.is_empty() || self.play_pos >= self.playlist.len() {
            self.reshuffle();
        }
        if self.playlist.is_empty() {
            return;
        }
        let real_index = self.playlist[self.play_pos];
        // Move the highlight to the sentence we're about to speak.
        self.selected = real_index; // <-- added line
        if let Some(text) = self.sentences.get(real_index).map(|s| s.text.clone()) {
            self.say_text(&text);
            self.speaking_index = Some(real_index);
        }
        self.play_pos += 1;
    }

    // The sentence most recently started by auto-play (the one being read, or
    // the last one read during the pause before the next). None before the
    // first sentence plays. Uses the playlist directly so it's correct
    // regardless of the current filter or selection.
    pub fn now_playing(&self) -> Option<&Sentence> {
        self.play_pos
            .checked_sub(1)
            .and_then(|p| self.playlist.get(p))
            .and_then(|&i| self.sentences.get(i))
    }

    // Move the highlight down by one (within the filtered list).
    pub fn next(&mut self) {
        let count = self.matches().len();
        if count == 0 {
            return; // nothing visible, nowhere to move
        }
        if self.selected + 1 >= count {
            self.selected = 0;
        } else {
            self.selected += 1;
        }
    }

    // Move the highlight up by one (within the filtered list).
    pub fn previous(&mut self) {
        let count = self.matches().len();
        if count == 0 {
            return;
        }
        if self.selected == 0 {
            self.selected = count - 1;
        } else {
            self.selected -= 1;
        }
    }

    // Speak the currently selected (visible) sentence.
    pub fn speak(&mut self) {
        let matches = self.matches();
        // selected is an index into `matches`. Look up the real index.
        let real = matches.get(self.selected).copied();
        if let Some(text) = real
            .and_then(|i| self.sentences.get(i))
            .map(|s| s.text.clone())
        {
            self.say_text(&text);
            self.speaking_index = real;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentence(text: &str, tags: &[&str]) -> Sentence {
        Sentence {
            text: text.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            note: String::new(),
            starred: false,
        }
    }

    fn app_with(sentences: Vec<Sentence>) -> App {
        App::new(Config {
            settings: Settings::default(),
            sentences,
        })
    }

    #[test]
    fn parse_tags_strips_hash_and_blanks() {
        assert_eq!(parse_tags("#past  work"), vec!["past", "work"]);
        assert_eq!(parse_tags("   "), Vec::<String>::new());
        assert_eq!(parse_tags(""), Vec::<String>::new());
    }

    #[test]
    fn empty_filter_matches_everything() {
        let app = app_with(vec![sentence("Hello", &["a"]), sentence("World", &["b"])]);
        assert_eq!(app.matches(), vec![0, 1]);
    }

    #[test]
    fn filter_matches_text_and_tags_case_insensitively() {
        let mut app = app_with(vec![
            sentence("She left early.", &["past", "work"]),
            sentence("How are you?", &["questions"]),
        ]);

        // Matches a tag.
        app.filter = "PAST".to_string();
        assert_eq!(app.matches(), vec![0]);

        // Matches the sentence text.
        app.filter = "how".to_string();
        assert_eq!(app.matches(), vec![1]);

        // No match.
        app.filter = "zzz".to_string();
        assert!(app.matches().is_empty());
    }

    #[test]
    fn starred_sentences_are_weighted_in_the_playlist() {
        let mut starred = sentence("Hard one", &[]);
        starred.starred = true;
        let mut app = app_with(vec![sentence("Easy one", &[]), starred]);

        app.reshuffle();

        // Index 1 (starred) appears 3x, index 0 once -> 4 entries total.
        assert_eq!(app.playlist.len(), 4);
        assert_eq!(app.playlist.iter().filter(|&&i| i == 0).count(), 1);
        assert_eq!(app.playlist.iter().filter(|&&i| i == 1).count(), 3);
    }

    #[test]
    fn star_weight_is_configurable() {
        let mut starred = sentence("Hard one", &[]);
        starred.starred = true;
        let mut app = App::new(Config {
            settings: Settings {
                star_weight: Some(5),
                ..Settings::default()
            },
            sentences: vec![sentence("Easy one", &[]), starred],
        });

        app.reshuffle();

        assert_eq!(app.playlist.iter().filter(|&&i| i == 1).count(), 5);
    }
}
