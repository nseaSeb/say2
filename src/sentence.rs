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

// Optional settings, read from a `[settings]` table at the top of the file.
// `voice` and `rate` map straight onto `say` flags: `-v <voice>` and
// `-r <rate>` (words per minute). `star_weight` controls how many times a
// starred sentence is weighted in the auto-play shuffle. Absent fields fall
// back to defaults.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub star_weight: Option<u32>,
}

impl Settings {
    fn is_default(&self) -> bool {
        self.voice.is_none() && self.rate.is_none() && self.star_weight.is_none()
    }
}

// Everything loaded from the config file.
pub struct Config {
    pub settings: Settings,
    pub sentences: Vec<Sentence>,
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
        assert_eq!(parsed.sentence.len(), 1);
        assert_eq!(parsed.sentence[0].note, "a note");
        assert!(parsed.sentence[0].starred);
    }
}
