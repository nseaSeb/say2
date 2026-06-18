use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Sentence {
    pub text: String,
    pub tags: Vec<String>,
    // A free-form per-sentence note. Stored as a structured field (not a TOML
    // comment) so it survives serde round-trips on edit/delete. Optional:
    // missing in old files, and omitted from output when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    // Marked as "difficult": weighted to come up more often in auto-play.
    // Like `note`, a structured field so it survives serde round-trips.
    #[serde(default, skip_serializing_if = "is_false")]
    pub starred: bool,
}

// skip_serializing_if helper: omit the field when it's false (the default).
fn is_false(b: &bool) -> bool {
    !*b
}

// How the main screen is arranged. `Stacked` is a single vertical pile
// (header, detail, filter, then the list full-width); `Classic` is the
// list on the left with a detail panel on the right. Serialized as the
// lowercase variant name, e.g. `layout = "classic"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Classic,
    Stacked,
}

// Optional settings, read from a `[settings]` table at the top of the file.
// `voice` and `rate` map straight onto `say` flags: `-v <voice>` and
// `-r <rate>` (words per minute). `star_weight` controls how many times a
// starred sentence is weighted in the auto-play shuffle. `layout` picks the
// screen arrangement. Absent fields fall back to defaults.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub star_weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Layout>,
}

impl Settings {
    fn is_default(&self) -> bool {
        self.voice.is_none()
            && self.rate.is_none()
            && self.star_weight.is_none()
            && self.layout.is_none()
    }
}

// Everything loaded from the config file.
pub struct Config {
    pub settings: Settings,
    pub sentences: Vec<Sentence>,
}

// Practice statistics, persisted in their own file (separate from the sentence
// library, which gets rewritten on every edit). All counters default to zero so
// an old or missing file just starts fresh. `last_active_day` is a day number
// (days since the Unix epoch) used to maintain the practice streak.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Stats {
    #[serde(default)]
    pub sentences_spoken: u64,
    #[serde(default)]
    pub play_secs: u64,
    #[serde(default)]
    pub sessions: u64,
    #[serde(default)]
    pub days_practiced: u64,
    #[serde(default)]
    pub current_streak: u64,
    #[serde(default)]
    pub best_streak: u64,
    #[serde(default)]
    pub last_active_day: Option<u64>,
    // Per-day usage history, kept in day order (oldest first), so we can draw
    // how usage evolves over time. Each entry is one calendar day with at least
    // some activity; days with none are simply absent.
    #[serde(default)]
    pub daily: Vec<DayStat>,
}

// One day's worth of usage. `day` is the day number (days since the Unix
// epoch), matching `Stats::last_active_day`.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct DayStat {
    pub day: u64,
    #[serde(default)]
    pub sentences: u64,
    #[serde(default)]
    pub play_secs: u64,
}

// Today as a day number: whole days since the Unix epoch (UTC). Cheap to
// compute from the system clock and enough to detect "same day" and
// "consecutive day" for the streak without pulling in a date library.
fn today_number() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

impl Stats {
    // Register that the user practiced today: bump the day count once per day
    // and grow the streak when today directly follows the last active day
    // (otherwise the streak resets to 1). Idempotent within the same day.
    pub fn touch_today(&mut self) {
        let today = today_number();
        match self.last_active_day {
            Some(d) if d == today => return, // already counted today
            Some(d) if d + 1 == today => self.current_streak += 1,
            _ => self.current_streak = 1,
        }
        self.days_practiced += 1;
        self.last_active_day = Some(today);
        self.best_streak = self.best_streak.max(self.current_streak);
    }

    // Today's history entry, appending a fresh one if the most recent isn't
    // today. Entries stay in day order because time only moves forward.
    fn today_entry(&mut self) -> &mut DayStat {
        let today = today_number();
        if self.daily.last().map(|d| d.day) != Some(today) {
            self.daily.push(DayStat {
                day: today,
                ..DayStat::default()
            });
        }
        self.daily.last_mut().expect("just ensured non-empty")
    }

    // Count one spoken sentence, both in the total and in today's history.
    pub fn record_sentence(&mut self) {
        self.sentences_spoken += 1;
        self.today_entry().sentences += 1;
    }

    // Add played seconds, both to the total and to today's history.
    pub fn add_play_secs(&mut self, secs: u64) {
        if secs == 0 {
            return;
        }
        self.play_secs += secs;
        self.today_entry().play_secs += secs;
    }

    // The last `n` calendar days up to today, oldest first, with missing days
    // filled in as zero so a chart shows real gaps. `n` is clamped to at least 1.
    pub fn recent_days(&self, n: u64) -> Vec<DayStat> {
        let today = today_number();
        let start = today.saturating_sub(n.max(1) - 1);
        (start..=today)
            .map(|day| {
                self.daily
                    .iter()
                    .find(|d| d.day == day)
                    .cloned()
                    .unwrap_or(DayStat {
                        day,
                        sentences: 0,
                        play_secs: 0,
                    })
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct SentenceFile {
    #[serde(default)]
    settings: Settings,
    sentence: Vec<Sentence>,
}

// Borrowed views used only for re-serializing the whole file.
#[derive(Serialize)]
struct SentenceFileRef<'a> {
    sentence: &'a [Sentence],
}

#[derive(Serialize)]
struct ConfigRef<'a> {
    settings: &'a Settings,
    sentence: &'a [Sentence],
}

// A small default set, embedded in the binary at compile time.
// Used to create the config file on first run, so the tool works
// immediately even before the user has written their own sentences.
const DEFAULT_SENTENCES: &str = r#"# Optional settings (uncomment and tweak): a specific macOS voice, a speaking
# rate in words per minute, and how strongly starred sentences are weighted in
# the auto-play shuffle (default 3):
# [settings]
# voice = "Daniel"
# rate = 170
# star_weight = 3

[[sentence]]
text = "Hey, how's it going?"
tags = ["greetings", "smalltalk"]

[[sentence]]
text = "What time do you usually wake up?"
tags = ["questions", "daily", "third-person-s"]

[[sentence]]
text = "I've already finished my work."
tags = ["present-perfect", "past-participle"]
"#;

// Build the path: ~/.config/say2/sentences.toml
fn config_path() -> PathBuf {
    let mut path = dirs::home_dir().expect("could not find home directory");
    path.push(".config");
    path.push("say2");
    path.push("sentences.toml");
    path
}

// Build the path: ~/.config/say2/stats.toml
fn stats_path() -> PathBuf {
    let mut path = dirs::home_dir().expect("could not find home directory");
    path.push(".config");
    path.push("say2");
    path.push("stats.toml");
    path
}

// Load practice stats. A missing or unreadable/unparseable file just yields
// the default (all-zero) stats, so a corrupt file never blocks the app.
pub fn load_stats() -> Stats {
    fs::read_to_string(stats_path())
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

// Persist practice stats, creating the config directory if needed.
pub fn save_stats(stats: &Stats) -> io::Result<()> {
    let path = stats_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let body = toml::to_string(stats).map_err(io::Error::other)?;
    fs::write(path, body)
}

// Load settings and sentences from the config file.
// On first run (file missing), create it with the default set.
pub fn load() -> Config {
    let path = config_path();

    // If the file doesn't exist yet, create the folder and write defaults.
    if !path.exists() {
        // path.parent() is the .../say2/ directory.
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).expect("could not create config directory");
        }
        fs::write(&path, DEFAULT_SENTENCES).expect("could not write default sentences");
        eprintln!(
            "Created {} — edit it to add your own sentences.",
            path.display()
        );
    }

    let raw =
        fs::read_to_string(&path).unwrap_or_else(|_| panic!("could not read {}", path.display()));

    let data: SentenceFile = toml::from_str(&raw)
        .unwrap_or_else(|e| panic!("could not parse {}: {}", path.display(), e));

    Config {
        settings: data.settings,
        sentences: data.sentence,
    }
}

// Escape a string for a TOML basic ("...") string: backslash and quote.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// Append a new [[sentence]] block to the end of the config file.
// We append (rather than re-serialize the whole file) so the user's
// comments and section grouping are left untouched.
pub fn append(text: &str, tags: &[String], note: &str) -> io::Result<()> {
    let tags_list = tags
        .iter()
        .map(|t| format!("\"{}\"", toml_escape(t)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut block = format!(
        "\n[[sentence]]\ntext = \"{}\"\ntags = [{}]\n",
        toml_escape(text),
        tags_list
    );
    // Only write the note line when there is one, mirroring skip_serializing_if.
    if !note.is_empty() {
        block.push_str(&format!("note = \"{}\"\n", toml_escape(note)));
    }

    let mut file = fs::OpenOptions::new().append(true).open(config_path())?;
    file.write_all(block.as_bytes())
}

// Rewrite the whole config file from the current in-memory state.
// Needed for edit/delete (we can't surgically patch one block), so this
// normalizes the file and drops any hand-written comments/section layout.
// Settings are preserved, and the `[settings]` table is omitted when empty.
pub fn save_all(settings: &Settings, sentences: &[Sentence]) -> io::Result<()> {
    let body = if settings.is_default() {
        toml::to_string(&SentenceFileRef {
            sentence: sentences,
        })
    } else {
        toml::to_string(&ConfigRef {
            settings,
            sentence: sentences,
        })
    }
    .map_err(io::Error::other)?;
    fs::write(config_path(), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(toml_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(toml_escape("plain"), "plain");
    }

    #[test]
    fn parses_settings_and_defaults_missing_note() {
        let raw = r#"
            [settings]
            voice = "Daniel"
            rate = 150

            [[sentence]]
            text = "Hi"
            tags = ["greetings"]
        "#;
        let f: SentenceFile = toml::from_str(raw).unwrap();
        assert_eq!(f.settings.voice.as_deref(), Some("Daniel"));
        assert_eq!(f.settings.rate, Some(150));
        assert_eq!(f.sentence.len(), 1);
        assert_eq!(f.sentence[0].note, ""); // note absent -> default empty
    }

    #[test]
    fn touch_today_starts_a_streak_and_is_idempotent_within_a_day() {
        let mut stats = Stats::default();
        stats.touch_today();
        assert_eq!(stats.days_practiced, 1);
        assert_eq!(stats.current_streak, 1);
        assert_eq!(stats.best_streak, 1);

        // A second call the same day must not double-count.
        stats.touch_today();
        assert_eq!(stats.days_practiced, 1);
        assert_eq!(stats.current_streak, 1);
    }

    #[test]
    fn touch_today_extends_streak_on_consecutive_day_and_resets_after_a_gap() {
        let today = today_number();

        // Yesterday was the last active day: today extends the streak.
        let mut stats = Stats {
            current_streak: 3,
            best_streak: 3,
            days_practiced: 3,
            last_active_day: Some(today - 1),
            ..Stats::default()
        };
        stats.touch_today();
        assert_eq!(stats.current_streak, 4);
        assert_eq!(stats.best_streak, 4);
        assert_eq!(stats.days_practiced, 4);

        // A two-day gap resets the streak to 1 (best is preserved).
        let mut stats = Stats {
            current_streak: 5,
            best_streak: 9,
            days_practiced: 20,
            last_active_day: Some(today - 2),
            ..Stats::default()
        };
        stats.touch_today();
        assert_eq!(stats.current_streak, 1);
        assert_eq!(stats.best_streak, 9);
        assert_eq!(stats.days_practiced, 21);
    }

    #[test]
    fn recording_usage_updates_totals_and_todays_history() {
        let mut stats = Stats::default();
        stats.record_sentence();
        stats.record_sentence();
        stats.add_play_secs(90);
        stats.add_play_secs(0); // ignored

        assert_eq!(stats.sentences_spoken, 2);
        assert_eq!(stats.play_secs, 90);
        // A single history entry for today carries the same figures.
        assert_eq!(stats.daily.len(), 1);
        assert_eq!(stats.daily[0].day, today_number());
        assert_eq!(stats.daily[0].sentences, 2);
        assert_eq!(stats.daily[0].play_secs, 90);
    }

    #[test]
    fn recent_days_fills_gaps_and_keeps_order() {
        let today = today_number();
        let stats = Stats {
            daily: vec![
                DayStat {
                    day: today - 3,
                    sentences: 5,
                    play_secs: 60,
                },
                DayStat {
                    day: today,
                    sentences: 2,
                    play_secs: 30,
                },
            ],
            ..Stats::default()
        };

        let window = stats.recent_days(4); // today-3 .. today
        assert_eq!(window.len(), 4);
        assert_eq!(window[0].day, today - 3);
        assert_eq!(window[0].sentences, 5);
        // The missing middle days are filled with zeros.
        assert_eq!(window[1].sentences, 0);
        assert_eq!(window[2].sentences, 0);
        assert_eq!(window[3].day, today);
        assert_eq!(window[3].sentences, 2);
    }

    #[test]
    fn settings_absent_is_default() {
        let raw = r#"
            [[sentence]]
            text = "Hi"
            tags = []
        "#;
        let f: SentenceFile = toml::from_str(raw).unwrap();
        assert!(f.settings.is_default());
    }

    #[test]
    fn settings_survive_serialize_then_parse() {
        let settings = Settings {
            voice: Some("Daniel".to_string()),
            rate: Some(150),
            star_weight: Some(4),
            layout: Some(Layout::Classic),
        };
        let sentences = vec![Sentence {
            text: "Hi".to_string(),
            tags: vec!["greetings".to_string()],
            note: "a note".to_string(),
            starred: true,
        }];

        // Mirror what save_all writes, then read it back.
        let body = toml::to_string(&ConfigRef {
            settings: &settings,
            sentence: &sentences,
        })
        .unwrap();
        let parsed: SentenceFile = toml::from_str(&body).unwrap();

        assert_eq!(parsed.settings.voice.as_deref(), Some("Daniel"));
        assert_eq!(parsed.settings.rate, Some(150));
        assert_eq!(parsed.settings.star_weight, Some(4));
        assert_eq!(parsed.settings.layout, Some(Layout::Classic));
        assert_eq!(parsed.sentence.len(), 1);
        assert_eq!(parsed.sentence[0].note, "a note");
        assert!(parsed.sentence[0].starred);
    }
}
