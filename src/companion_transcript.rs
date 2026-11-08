//! Exact speech remains source data. Presentation groups fragments without
//! turning the provider's timing anchor into a completed conversational turn.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(super) struct Fragment {
    pub seq: i64,
    pub event_id: Option<String>,
    pub role: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(super) struct Segment {
    pub role: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub fragment_count: usize,
    pub crosses_delegation_anchor: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(super) struct Source {
    pub version: u8,
    pub previous_anchor_ms: i64,
    pub offset_ms: i64,
    pub fragments: Vec<Fragment>,
}

pub(super) fn fragments(rows: Vec<(i64, String)>) -> Result<Vec<Fragment>> {
    rows.into_iter()
        .map(|(seq, data)| {
            let value: Value = serde_json::from_str(&data)?;
            Ok(Fragment {
                seq,
                event_id: value["event_id"].as_str().map(str::to_owned),
                role: value["role"].as_str().unwrap_or("").to_owned(),
                start_ms: value["start_ms"].as_i64().unwrap_or(0),
                end_ms: value["end_ms"].as_i64().unwrap_or(0),
                text: value["text"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

pub(super) fn group(fragments: &[Fragment], anchor: i64) -> Vec<Segment> {
    let mut groups: Vec<Segment> = Vec::new();
    for f in fragments {
        // Group arrival-adjacent speech from the same speaker. Text is joined
        // verbatim: inserted punctuation/spacing would change the source.
        if let Some(last) = groups.last_mut()
            && last.role == f.role
            && f.start_ms >= last.start_ms
            && f.start_ms <= last.end_ms.saturating_add(3000)
        {
            last.text.push_str(&f.text);
            last.end_ms = last.end_ms.max(f.end_ms);
            last.fragment_count += 1;
            last.crosses_delegation_anchor |= f.end_ms > anchor;
        } else {
            groups.push(Segment {
                role: f.role.clone(),
                start_ms: f.start_ms,
                end_ms: f.end_ms,
                text: f.text.clone(),
                fragment_count: 1,
                crosses_delegation_anchor: f.end_ms > anchor,
            });
        }
    }
    groups
}

impl Source {
    pub fn pending(
        all: &[Fragment],
        previous: i64,
        legacy_previous: i64,
        offset: i64,
        consumed: &HashSet<i64>,
    ) -> Self {
        Self {
            version: 1,
            previous_anchor_ms: previous,
            offset_ms: offset,
            fragments: all
                .iter()
                .filter(|f| {
                    f.start_ms > legacy_previous
                        && f.start_ms <= offset
                        && !consumed.contains(&f.seq)
                })
                .cloned()
                .collect(),
        }
    }
    #[cfg(test)]
    pub fn select(all: &[Fragment], previous: i64, offset: i64) -> Self {
        Self {
            version: 1,
            previous_anchor_ms: previous,
            offset_ms: offset,
            fragments: all
                .iter()
                .filter(|f| f.start_ms > previous && f.start_ms <= offset)
                .cloned()
                .collect(),
        }
    }
    pub fn human(&self) -> bool {
        self.fragments
            .iter()
            .any(|f| f.role == "user" && !f.text.trim().is_empty())
    }
    pub fn presentation(&self) -> Value {
        json!(group(&self.fragments, self.offset_ms))
    }
    pub fn label(&self) -> String {
        group(&self.fragments, self.offset_ms)
            .into_iter()
            .filter(|f| f.role == "user" && !f.text.trim().is_empty())
            .map(|f| f.text.trim().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(super) fn recent(all: &[Fragment], previous: i64) -> Value {
    let before = all
        .iter()
        .filter(|f| f.start_ms <= previous)
        .cloned()
        .collect::<Vec<_>>();
    let groups = group(&before, previous);
    let mut bytes = 0;
    let mut selected = Vec::new();
    for group in groups.iter().rev().take(6) {
        let size = serde_json::to_vec(group).unwrap().len();
        if bytes + size > 8000 {
            break;
        }
        bytes += size;
        selected.push(group);
    }
    selected.reverse();
    json!({"segments":selected,"omitted":selected.len()<groups.len(),"action_authority":false})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(seq: i64, role: &str, text: &str) -> Fragment {
        Fragment {
            seq,
            event_id: Some(format!("source-{seq}")),
            role: role.into(),
            start_ms: seq * 200,
            end_ms: seq * 200 + 200,
            text: text.into(),
        }
    }

    #[test]
    fn long_clear_instruction_keeps_every_word_and_exact_mapping_without_word_metadata_overhead() {
        let all = (1..=700)
            .map(|i| {
                word(
                    i,
                    "user",
                    if i == 699 {
                        " without changing the roof"
                    } else {
                        " continue"
                    },
                )
            })
            .collect::<Vec<_>>();
        let source = Source::select(&all, -1, 140_000);
        let original = serde_json::to_vec(&source).unwrap();
        assert!(original.len() > 48 * 1024);
        let compact = source.presentation();
        assert!(compact.to_string().len() < 10_000);
        assert_eq!(
            compact[0]["text"],
            all.iter().map(|f| f.text.as_str()).collect::<String>()
        );
        assert_eq!(source.fragments, all);
        assert!(
            compact[0]["text"]
                .as_str()
                .unwrap()
                .contains("without changing the roof")
        );
        assert_eq!(serde_json::from_slice::<Source>(&original).unwrap(), source);
    }

    #[test]
    fn anchor_straddle_yes_correction_and_recent_speaker_roles_remain_exact() {
        let mut all = vec![
            word(1, "user", "Inspect the roof."),
            word(2, "assistant", "Shall I save that?"),
            word(3, "user", "Yes — but keep it private."),
        ];
        all[0].end_ms = 450;
        let first = Source::select(&all, -1, 200);
        assert_eq!(first.fragments.len(), 1);
        assert_eq!(first.presentation()[0]["crosses_delegation_anchor"], true);
        let next = Source::select(&all, 200, 600);
        assert_eq!(next.fragments.len(), 2);
        assert_eq!(next.label(), "Yes — but keep it private.");
        assert_eq!(recent(&all, 400)["segments"][1]["role"], "assistant");
        assert_eq!(next.presentation()[1]["text"], "Yes — but keep it private.");
    }

    #[test]
    fn late_observed_correction_is_pending_until_owned_by_an_admitted_source() {
        let all = vec![
            word(1, "user", "Paint the wall"),
            word(2, "user", " blue, not red."),
            word(4, "user", "Please check that correction."),
        ];
        let pending = Source::pending(&all, 600, -1, 800, &HashSet::from([1]));
        assert_eq!(
            pending.fragments.iter().map(|f| f.seq).collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert!(pending.label().contains("blue, not red"));
        assert!(!pending.label().contains("Paint the wall"));
    }
}
