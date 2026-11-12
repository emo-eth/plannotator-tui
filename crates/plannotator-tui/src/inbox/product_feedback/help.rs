use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

#[derive(Debug, Deserialize)]
pub(super) struct Action {
    pub(super) kind: String,
    pub(super) label: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Link {
    pub(super) label: String,
    pub(super) href: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Faq {
    pub(super) question: String,
    pub(super) answer: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Matching {
    #[serde(default)]
    phrases: Vec<String>,
    #[serde(default)]
    near: Vec<Vec<String>>,
    #[serde(default)]
    when_broken: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Known {
    pub(super) id: String,
    pub(super) answer: String,
    fixed_in: Option<String>,
    #[serde(default)]
    experimental: bool,
    pub(super) action: Option<Action>,
    pub(super) link: Option<Link>,
    pub(super) faq: Option<Faq>,
    feature: Option<String>,
    #[serde(rename = "match")]
    matching: Matching,
}

impl Known {
    pub(super) fn label(&self) -> String {
        if let Some(version) = &self.fixed_in {
            format!("Fixed in {version}: {}", self.answer)
        } else if self.experimental {
            format!("Experimental: {}", self.answer)
        } else {
            self.answer.clone()
        }
    }

    fn visible(&self, settings: Option<&Value>) -> bool {
        let Some(settings) = settings else { return true };
        let field = match self.id.as_str() {
            "start-agents" => return settings.pointer("/launch/unsupported") != Some(&Value::Bool(true)),
            "board" => "board",
            "checklist" => "steps",
            "bro-mode" => "bro",
            "float-drafts" => "float_drafts",
            "decision-mode" => "experimental",
            _ => return true,
        };
        settings.get(field).is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Feature {
    pub(super) title: String,
    pub(super) definition: String,
    names: Vec<String>,
    pub(super) url: String,
    #[serde(skip)]
    title_words: Vec<String>,
    #[serde(skip)]
    definition_words: Vec<String>,
    #[serde(skip)]
    name_words: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    features: Vec<Feature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hit {
    Known(usize),
    Feature(usize),
}

#[derive(Debug)]
pub(super) struct Help {
    pub(super) known: Vec<Known>,
    pub(super) features: Vec<Feature>,
}

impl Help {
    pub(super) fn load() -> Result<Self> {
        let mut known: Vec<Known> = serde_json::from_str(include_str!("known-answers.json"))
            .context("reading bundled feedback answers")?;
        for answer in &mut known {
            for phrase in &mut answer.matching.phrases {
                *phrase = fold(phrase);
            }
            for group in &mut answer.matching.near {
                for phrase in group {
                    *phrase = fold(phrase);
                }
            }
        }
        let mut data: SearchData = serde_json::from_str(include_str!("search-data.json"))
            .context("reading bundled feedback feature help")?;
        for feature in &mut data.features {
            feature.title_words = words_of(&feature.title, false);
            feature.definition_words = words_of(&feature.definition, false);
            feature.name_words = feature.names.iter().map(|n| words_of(n, true)).collect();
        }
        Ok(Self { known, features: data.features })
    }

    pub(super) fn related(&self, text: &str, settings: Option<&Value>) -> Vec<Hit> {
        let text: String = text.chars().take(super::model::MESSAGE_MAX).collect();
        let folded = fold(&text);
        if folded.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = folded.split_whitespace().collect();
        let broken = is_broken(&words);
        let known = self
            .known
            .iter()
            .position(|answer| answer.visible(settings) && matches(&answer.matching, &words, broken));
        let duplicate = known.and_then(|i| self.known.get(i)).and_then(|a| a.feature.as_deref());
        let typed = words_of(&text, false);
        let all = words_of(&text, true);
        let mut features: Vec<(usize, f64)> = self
            .features
            .iter()
            .enumerate()
            .filter_map(|(index, feature)| {
                if duplicate == Some(feature.title.as_str()) || typed.is_empty() {
                    return None;
                }
                let named = feature.name_words.iter().any(|name| {
                    !name.is_empty() && name.iter().all(|word| all.iter().any(|other| same_word(word, other)))
                });
                let score = if named {
                    f64::INFINITY
                } else {
                    closeness(&typed, &feature.title_words, feature.title_words.len().clamp(1, 2))
                        .max(closeness(&typed, &feature.definition_words, 2))
                };
                (score > 0.0).then_some((index, score))
            })
            .collect();
        features.sort_by(|a, b| b.1.total_cmp(&a.1));
        known
            .into_iter()
            .map(Hit::Known)
            .chain(features.into_iter().map(|(index, _)| Hit::Feature(index)))
            .take(5)
            .collect()
    }

    pub(super) fn label(&self, hit: Hit) -> String {
        match hit {
            Hit::Known(index) => self.known.get(index).map_or_else(String::new, Known::label),
            Hit::Feature(index) => self
                .features
                .get(index)
                .map_or_else(String::new, |f| format!("{}: {}", f.title, f.definition)),
        }
    }
}

fn fold(text: &str) -> String {
    let normalized: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            '’' | '‘' => '\'',
            c if c.is_alphanumeric() || c == '\'' => c,
            _ => ' ',
        })
        .collect();
    normalized
        .split_whitespace()
        .map(|word| word.trim_matches('\''))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn phrase_at(words: &[&str], at: usize, phrase: &str) -> bool {
    let mut count = 0;
    for (offset, part) in phrase.split_whitespace().enumerate() {
        if words.get(at + offset).copied() != Some(part) {
            return false;
        }
        count += 1;
    }
    count > 0
}

fn contains(words: &[&str], phrase: &str) -> bool {
    (0..words.len()).any(|at| phrase_at(words, at, phrase))
}

fn negated(words: &[&str], at: usize) -> bool {
    const NEGATIONS: &[&str] =
        &["doesn't", "does not", "don't", "do not", "never", "no longer", "not", "stopped", "stop"];
    (at.saturating_sub(3)..at).any(|start| {
        NEGATIONS.iter().any(|term| {
            let end = start + term.split_whitespace().count();
            phrase_at(words, start, term) && end <= at && at - end <= 1
        })
    })
}

fn is_broken(words: &[&str]) -> bool {
    const BROKEN: &[&str] = &[
        "broken", "broke", "breaks", "crash", "crashes", "crashed", "crashing", "error", "errors", "fails",
        "failed", "failing", "bug", "buggy", "freezes", "froze", "frozen", "hangs", "blank",
    ];
    const NEGATIONS: &[&str] = &[
        "doesn't", "does not", "don't", "do not", "won't", "will not", "can't", "cannot", "can not", "isn't",
        "is not", "aren't", "are not", "wasn't", "stopped",
    ];
    const VERBS: &[&str] = &[
        "work", "works", "working", "open", "opens", "load", "loads", "show", "shows", "showing", "start",
        "starts", "run", "runs", "launch", "appear", "appears",
    ];
    words.iter().any(|w| BROKEN.contains(w))
        || (0..words.len()).any(|at| {
            NEGATIONS.iter().any(|term| {
                phrase_at(words, at, term)
                    && words.get(at + term.split_whitespace().count()).is_some_and(|w| VERBS.contains(w))
            })
        })
}

fn matches(matching: &Matching, words: &[&str], broken: bool) -> bool {
    if broken && !matching.when_broken {
        return false;
    }
    if matching.phrases.iter().any(|phrase| contains(words, phrase)) {
        return true;
    }
    let Some(first) = matching.near.first() else { return false };
    (0..words.len())
        .filter(|at| !negated(words, *at) && first.iter().any(|term| phrase_at(words, *at, term)))
        .any(|start| {
            matching.near.iter().skip(1).all(|group| {
                (start.saturating_sub(8)..(start + 9).min(words.len()))
                    .any(|at| !negated(words, at) && group.iter().any(|term| phrase_at(words, at, term)))
            })
        })
}

const FILLER: &str = "about above actually after again against almost also always another anyone anything anyway around away back because been before being below better between both cannot could couldn didn does doesn doing down during each either else even ever every everything example feature features from further getting gets give given going gone good great hasn have haven having hello here hope however idea ideas instead into isn issue issues just keep keeps kept kind know less like likely lots love made make makes making many maybe more most much must need needed needs never nice only other others over please plannotator inbox pretty probably problem problems quite rather really request requests right same seem seems should shouldn since some someone something sometimes somewhere soon still such sure than thank thanks that their them then there these they thing things think this those though through together totally very want wanted wants wasn well were what when where whether which while will wish with within without work worked working works would wouldn yeah your yours";

fn words_of(text: &str, all: bool) -> Vec<String> {
    let folded = fold(text);
    let mut result = Vec::new();
    for word in folded.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        if !all && (word.chars().count() < 4 || FILLER.split_whitespace().any(|w| w == word)) {
            continue;
        }
        if !result.iter().any(|w| w == word) {
            result.push(word.to_owned());
        }
    }
    result
}

fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    if a_len < 4 || b_len < 4 {
        return false;
    }
    if (a.starts_with(b) || b.starts_with(a)) && a_len.abs_diff(b_len) <= 3 {
        return true;
    }
    if a_len < 5 || b_len < 5 || a_len.abs_diff(b_len) > 1 {
        return false;
    }
    let mut a_chars = a.chars();
    let mut b_chars = b.chars();
    loop {
        let a_rest = a_chars.as_str();
        let b_rest = b_chars.as_str();
        let left = a_chars.next();
        let right = b_chars.next();
        if left != right {
            return a_chars.as_str() == b_chars.as_str()
                || a_chars.as_str() == b_rest
                || a_rest == b_chars.as_str();
        }
        if left.is_none() {
            return true;
        }
    }
}

fn closeness(typed: &[String], entry: &[String], least: usize) -> f64 {
    let common = typed.iter().filter(|word| entry.iter().any(|other| same_word(word, other))).count();
    if common >= least
        && !entry.is_empty()
        && common as f64 / typed.len().min(entry.len()).max(1) as f64 >= 0.6
    {
        common as f64 + common as f64 / entry.len() as f64
    } else {
        0.0
    }
}
