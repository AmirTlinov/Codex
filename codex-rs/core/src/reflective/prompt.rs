use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use super::model::ReflectiveConfidence;
use super::model::ReflectiveDisposition;
use super::model::ReflectiveObservation;
use super::model::ReflectiveObservationCategory;

const REFLECTIVE_POLICY_PROMPT: &str = concat!(
    "You are an internal reflective maintenance sidecar for Codex.\n",
    "Your job is not to continue execution of the task. Your job is to inspect the current thread and extract only the highest-signal observations that could improve the next real turn.\n",
    "Focus on non-obvious blind spots, contradictions, integration risks, subtle hidden assumptions, and unusually high-upside ideas.\n",
    "Do not produce chain-of-thought. Do not suggest broad rewrites. Do not repeat the obvious plan.\n",
    "Return strict JSON only.\n",
);

const REFLECTIVE_USER_PROMPT: &str = concat!(
    "Refresh the reflective window for this thread.\n",
    "Return at most five observations.\n",
    "Only include observations that are still fresh and actionable for the next real turn.\n",
    "Prefer `watch`, `verify`, or `promote`. Use `discard` only when an item should be dropped entirely.\n",
);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ReflectiveReport {
    pub(crate) observations: Vec<ReflectiveObservation>,
}

pub(crate) fn reflective_policy_prompt() -> &'static str {
    REFLECTIVE_POLICY_PROMPT
}

pub(crate) fn reflective_user_prompt() -> String {
    REFLECTIVE_USER_PROMPT.to_string()
}

pub(crate) fn reflective_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["observations"],
        "properties": {
            "observations": {
                "type": "array",
                "maxItems": 5,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "category",
                        "note",
                        "why_it_matters",
                        "evidence",
                        "confidence",
                        "disposition"
                    ],
                    "properties": {
                        "category": {
                            "type": "string",
                            "enum": [
                                ReflectiveObservationCategory::BlindSpot.as_str(),
                                ReflectiveObservationCategory::Risk.as_str(),
                                ReflectiveObservationCategory::Inconsistency.as_str(),
                                ReflectiveObservationCategory::Hypothesis.as_str(),
                                ReflectiveObservationCategory::Opportunity.as_str(),
                            ]
                        },
                        "note": { "type": "string" },
                        "why_it_matters": { "type": "string" },
                        "evidence": { "type": "string" },
                        "confidence": {
                            "type": "string",
                            "enum": [
                                ReflectiveConfidence::Low.as_str(),
                                ReflectiveConfidence::Medium.as_str(),
                                ReflectiveConfidence::High.as_str(),
                            ]
                        },
                        "disposition": {
                            "type": "string",
                            "enum": [
                                ReflectiveDisposition::Watch.as_str(),
                                ReflectiveDisposition::Verify.as_str(),
                                ReflectiveDisposition::Promote.as_str(),
                                ReflectiveDisposition::Discard.as_str(),
                            ]
                        }
                    }
                }
            }
        }
    })
}
