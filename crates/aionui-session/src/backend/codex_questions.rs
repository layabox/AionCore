//! Codex native question mapping. Verified against the installed codex-cli 0.161.0
//! `app-server generate-json-schema --experimental` output, checked in under
//! docs/protocol-schemas/codex-0.161.0/ToolRequestUserInput{Params,Response}.json.
//! Raw question IDs stay here; the shared Ask/AnswerAsk contract uses display text.

use super::types::{BackendError, QuestionAnswer};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(super) enum PendingCodexRequest {
    Approval(String),
    Question(CodexQuestionRequest),
    // Kept until serverRequest/resolved to absorb the server's acknowledgement
    // without decrementing either the question or permission counter twice.
    AnsweredQuestion { wire_id: Value },
}

impl PendingCodexRequest {
    pub fn question_wire_id(&self) -> Option<&Value> {
        match self {
            Self::Question(request) => Some(&request.wire_id),
            Self::AnsweredQuestion { wire_id } => Some(wire_id),
            Self::Approval(_) => None,
        }
    }
}

pub(super) type PendingCodexRequests = std::sync::Arc<std::sync::Mutex<HashMap<String, PendingCodexRequest>>>;

#[derive(Clone)]
pub(super) struct CodexQuestionRequest {
    pub wire_id: Value,
    pub turn_gen: u64,
    pub blocking: bool,
    questions: Vec<Question>,
}

#[derive(Clone, Deserialize)]
struct Question {
    id: String,
    header: String,
    question: String,
    #[serde(default)]
    options: Option<Vec<QuestionOption>>,
    #[serde(default, rename = "isSecret")]
    secret: bool,
}

#[derive(Clone, Deserialize)]
struct QuestionOption {
    label: String,
    description: String,
}

#[derive(Deserialize)]
struct Params {
    questions: Vec<Question>,
    #[serde(rename = "isBlocking")]
    blocking: Option<bool>,
}

impl CodexQuestionRequest {
    pub fn parse(wire_id: Value, params: Value, turn_gen: u64) -> Result<Self, BackendError> {
        let invalid = || BackendError::Transport("invalid Codex requestUserInput payload".into());
        if !wire_id.is_string() && wire_id.as_i64().is_none() {
            return Err(invalid());
        }
        let Params {
            mut questions,
            blocking,
        } = serde_json::from_value(params).map_err(|_| invalid())?;
        let mut ids = HashSet::new();
        if questions.is_empty()
            || questions.iter().any(|q| {
                q.id.trim().is_empty()
                    || q.question.trim().is_empty()
                    || !ids.insert(q.id.clone())
                    || q.options.as_ref().is_some_and(|options| {
                        let mut labels = HashSet::new();
                        options
                            .iter()
                            .any(|o| o.label.trim().is_empty() || !labels.insert(o.label.clone()))
                    })
            })
        {
            return Err(invalid());
        }
        // The common answer channel is keyed by text (Claude's contract). Native
        // Codex IDs need not have unique text: disambiguate only collisions, keeping
        // an exact private text -> native ID mapping for the answer round-trip.
        let mut used: HashSet<String> = questions.iter().map(|q| q.question.clone()).collect();
        let mut seen = HashSet::new();
        for question in &mut questions {
            if !seen.insert(question.question.clone()) {
                let base = format!("{} [{}]", question.question, question.id);
                let mut display = base.clone();
                let mut suffix = 2;
                while !used.insert(display.clone()) {
                    display = format!("{base} ({suffix})");
                    suffix += 1;
                }
                question.question = display;
            }
        }
        Ok(Self {
            wire_id,
            turn_gen,
            blocking: blocking.unwrap_or(true),
            questions,
        })
    }

    pub fn card(&self) -> Value {
        Value::Array(
            self.questions
                .iter()
                .map(|q| {
                    json!({
                        "question": q.question, "header": q.header, "multiSelect": false,
                        "isSecret": q.secret,
                        "options": q.options.as_deref().unwrap_or_default().iter().map(|o|
                            json!({ "label": o.label, "description": o.description })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }

    pub fn response(&self, answers: Option<&[QuestionAnswer]>) -> Result<Value, BackendError> {
        let invalid = || BackendError::Transport("Codex question answer set is incomplete or invalid".into());
        let mut mapped = serde_json::Map::new();
        if let Some(answers) = answers {
            if answers.len() != self.questions.len() {
                return Err(invalid());
            }
            let mut seen = HashSet::new();
            for answer in answers {
                let question = self
                    .questions
                    .iter()
                    .find(|q| q.question == answer.question)
                    .ok_or_else(invalid)?;
                if !seen.insert(&question.id)
                    || answer.labels.is_empty()
                    || answer.labels.iter().any(|s| s.trim().is_empty())
                {
                    return Err(invalid());
                }
                mapped.insert(question.id.clone(), json!({ "answers": answer.labels }));
            }
        }
        // The native response schema has no decline field. An explicit UI decline
        // is the valid empty answer map; never fabricate an approval or pick option 1.
        Ok(json!({ "jsonrpc": "2.0", "id": self.wire_id, "result": { "answers": mapped } }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: Value) -> CodexQuestionRequest {
        CodexQuestionRequest::parse(id, json!({ "questions": [
            { "id": "scope", "header": "Scope", "question": "Where?", "options": [
                { "label": "UI", "description": "Keep in UI" }, { "label": "Activity", "description": "Move to activity" } ] },
            { "id": "name", "header": "Name", "question": "Name?", "options": null, "isSecret": true }
        ] }), 7).unwrap()
    }

    #[test]
    fn codex_questions_replays_real_codex_0161_question_and_response() {
        let frame: Value = serde_json::from_str(include_str!(
            "../../../../docs/protocol-schemas/codex-0.161.0/request-user-input.live.json"
        ))
        .unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../docs/protocol-schemas/codex-0.161.0/request-user-input-response.live.json"
        ))
        .unwrap();
        let request = CodexQuestionRequest::parse(frame["id"].clone(), frame["params"].clone(), 1).unwrap();
        let cards = request.card();
        let answers = cards
            .as_array()
            .unwrap()
            .iter()
            .map(|q| QuestionAnswer {
                question: q["question"].as_str().unwrap().into(),
                labels: vec![q["options"][0]["label"].as_str().unwrap().into()],
            })
            .collect::<Vec<_>>();
        let actual = request.response(Some(&answers)).unwrap();
        assert_eq!(actual["id"], expected["id"]);
        assert_eq!(actual["result"], expected["result"]);
    }

    #[test]
    fn codex_questions_map_all_ids_and_preserve_numeric_or_string_rpc_id() {
        for id in [json!(17), json!("request-17")] {
            let request = request(id.clone());
            let response = request
                .response(Some(&[
                    QuestionAnswer {
                        question: "Name?".into(),
                        labels: vec!["custom".into()],
                    },
                    QuestionAnswer {
                        question: "Where?".into(),
                        labels: vec!["UI".into()],
                    },
                ]))
                .unwrap();
            assert_eq!(response["id"], id);
            assert_eq!(
                response["result"],
                json!({ "answers": { "scope": { "answers": ["UI"] }, "name": { "answers": ["custom"] } } })
            );
            assert_eq!(request.card()[1]["isSecret"], true);
            assert_eq!(request.card()[1]["options"], json!([]));
            assert_eq!(request.response(None).unwrap()["result"], json!({ "answers": {} }));
        }
    }

    #[test]
    fn codex_questions_reject_incomplete_unknown_and_duplicate_answers() {
        let request = request(json!(9));
        let answer = QuestionAnswer {
            question: "Where?".into(),
            labels: vec!["UI".into()],
        };
        assert!(request.response(Some(&[])).is_err());
        assert!(request.response(Some(std::slice::from_ref(&answer))).is_err());
        assert!(request.response(Some(&[answer.clone(), answer.clone()])).is_err());
        assert!(
            request
                .response(Some(&[
                    answer,
                    QuestionAnswer {
                        question: "Unknown".into(),
                        labels: vec!["x".into()]
                    }
                ]))
                .is_err()
        );
    }

    #[test]
    fn codex_questions_duplicate_text_keeps_an_unambiguous_native_id_mapping() {
        let request = CodexQuestionRequest::parse(
            json!(1),
            json!({ "questions": [
            { "id": "a", "header": "A", "question": "Same?" },
            { "id": "b", "header": "B", "question": "Same?" },
            { "id": "c", "header": "C", "question": "Same? [b]" }
        ] }),
            0,
        )
        .unwrap();
        let cards = request.card();
        assert_eq!(cards[1]["question"], "Same? [b] (2)");
        let answers = cards
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, q)| QuestionAnswer {
                question: q["question"].as_str().unwrap().into(),
                labels: vec![i.to_string()],
            })
            .collect::<Vec<_>>();
        let response = request.response(Some(&answers)).unwrap();
        assert_eq!(response["result"]["answers"]["b"]["answers"], json!(["1"]));
    }

    #[test]
    fn codex_questions_invalid_requests_never_become_partial_cards() {
        for params in [
            json!({}),
            json!({ "questions": [] }),
            json!({ "questions": [
            { "id": "x", "header": "X", "question": "One?" }, { "id": "x", "header": "Y", "question": "Two?" }
        ] }),
            json!({ "questions": [{ "id": "x", "header": "X", "question": "?", "options": [{ "label": "" }] }] }),
        ] {
            assert!(CodexQuestionRequest::parse(json!(1), params, 0).is_err());
        }
    }
}
