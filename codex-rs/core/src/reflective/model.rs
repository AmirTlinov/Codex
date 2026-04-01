use chrono::Utc;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;

use crate::contextual_user_message::REFLECTIVE_WINDOW_FRAGMENT;

const MAX_OBSERVATIONS: usize = 5;
const MAX_TEXT_CHARS: usize = 280;
const MAX_EVIDENCE_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReflectiveObservationCategory {
    BlindSpot,
    Risk,
    Inconsistency,
    Hypothesis,
    Opportunity,
}

impl ReflectiveObservationCategory {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::BlindSpot => "blind_spot",
            Self::Risk => "risk",
            Self::Inconsistency => "inconsistency",
            Self::Hypothesis => "hypothesis",
            Self::Opportunity => "opportunity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReflectiveConfidence {
    Low,
    Medium,
    High,
}

impl ReflectiveConfidence {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReflectiveDisposition {
    Watch,
    Verify,
    Promote,
    Discard,
}

impl ReflectiveDisposition {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Verify => "verify",
            Self::Promote => "promote",
            Self::Discard => "discard",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ReflectiveObservation {
    pub(crate) category: ReflectiveObservationCategory,
    pub(crate) note: String,
    pub(crate) why_it_matters: String,
    pub(crate) evidence: String,
    pub(crate) confidence: ReflectiveConfidence,
    pub(crate) disposition: ReflectiveDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReflectiveWindowState {
    pub(crate) generated_at: String,
    pub(crate) source_turn_id: String,
    pub(crate) observations: Vec<ReflectiveObservation>,
}

impl ReflectiveWindowState {
    pub(crate) fn from_report(
        source_turn_id: String,
        report: super::prompt::ReflectiveReport,
    ) -> Option<Self> {
        let observations = report
            .observations
            .into_iter()
            .filter_map(normalize_observation)
            .take(MAX_OBSERVATIONS)
            .collect::<Vec<_>>();
        if observations.is_empty() {
            return None;
        }

        Some(Self {
            generated_at: Utc::now().to_rfc3339(),
            source_turn_id,
            observations,
        })
    }

    pub(crate) fn into_prompt_item(self) -> ResponseItem {
        let mut lines = vec![
            format!("  <generated_at>{}</generated_at>", self.generated_at),
            format!("  <source_turn_id>{}</source_turn_id>", self.source_turn_id),
        ];
        for observation in self.observations {
            lines.push(format!(
                "  <observation category=\"{}\" confidence=\"{}\" disposition=\"{}\">",
                observation.category.as_str(),
                observation.confidence.as_str(),
                observation.disposition.as_str(),
            ));
            lines.push(format!("    <note>{}</note>", observation.note));
            lines.push(format!("    <why>{}</why>", observation.why_it_matters));
            lines.push(format!("    <evidence>{}</evidence>", observation.evidence));
            lines.push("  </observation>".to_string());
        }
        let text = REFLECTIVE_WINDOW_FRAGMENT.wrap(lines.join("\n"));
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text }],
            end_turn: None,
            phase: None,
        }
    }
}

fn normalize_observation(observation: ReflectiveObservation) -> Option<ReflectiveObservation> {
    if observation.disposition == ReflectiveDisposition::Discard {
        return None;
    }

    let note = truncate_scalar(observation.note, MAX_TEXT_CHARS);
    let why_it_matters = truncate_scalar(observation.why_it_matters, MAX_TEXT_CHARS);
    let evidence = truncate_scalar(observation.evidence, MAX_EVIDENCE_CHARS);
    if note.is_empty() || why_it_matters.is_empty() {
        return None;
    }

    Some(ReflectiveObservation {
        category: observation.category,
        note,
        why_it_matters,
        evidence,
        confidence: observation.confidence,
        disposition: observation.disposition,
    })
}

fn truncate_scalar(text: String, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    for (index, ch) in trimmed.chars().enumerate() {
        if index >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}
