use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    assets::Image,
    error::{Error, Result},
};

// ---------------------------------------------------------------------------
// Questions
// ---------------------------------------------------------------------------

/// Descriptions of the yes and no outcomes of a `noul` question.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NoulCriteria {
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    pub yes: Option<Value>,
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    pub no: Option<Value>,
}

/// A question, identified on the wire by its `type` field.
///
/// Instructions and descriptions are any JSON value: text, an object, an array, or `null`.
/// Build them with [`noul`], [`choice`], [`score`], and [`locate`].
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// A yes/no question.
    Noul {
        instructions: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Selects between named alternatives; a `Null` description leaves a label undescribed.
    Choice {
        instructions: Option<Value>,
        criteria: BTreeMap<String, Value>,
    },
    /// Assigns a score from an ordered rubric indexed from zero.
    Score {
        instructions: Option<Value>,
        criteria: Vec<Value>,
    },
    /// Locates what the instructions describe in the request's image (TS `box`).
    Box { instructions: Option<Value> },
}

/// Create a yes/no question.
pub fn noul(instructions: impl Into<Value>) -> Question {
    Question::Noul {
        instructions: Some(instructions.into()),
        criteria: None,
    }
}

/// Create a question that selects between labels. Use `Value::Null` for an undescribed label.
pub fn choice<K: Into<String>, V: Into<Value>>(
    instructions: impl Into<Value>,
    criteria: impl IntoIterator<Item = (K, V)>,
) -> Question {
    Question::Choice {
        instructions: Some(instructions.into()),
        criteria: criteria
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    }
}

/// Create a score question; entry `i` describes score `i`. Needs at least two entries.
pub fn score(
    instructions: impl Into<Value>,
    criteria: impl IntoIterator<Item = impl Into<Value>>,
) -> Question {
    Question::Score {
        instructions: Some(instructions.into()),
        criteria: criteria.into_iter().map(Into::into).collect(),
    }
}

/// Create a `box` question that locates something in the image. Use model `jev-preview`.
pub fn locate(instructions: impl Into<Value>) -> Question {
    Question::Box {
        instructions: Some(instructions.into()),
    }
}

fn is_blank(v: &Option<Value>) -> bool {
    v.as_ref().is_none_or(Value::is_null)
}

/// Reject empty question sets, score questions with fewer than two criteria, choice questions
/// with no criteria, and noul questions with neither instructions nor a described outcome.
pub(crate) fn validate_questions(questions: &BTreeMap<String, Question>) -> Result<()> {
    if questions.is_empty() {
        return Err(Error::sdk("At least one question is required."));
    }
    for (name, q) in questions {
        match q {
            Question::Choice { criteria, .. } if criteria.is_empty() => {
                return Err(Error::sdk(format!(
                    "Choice question \"{name}\" has no criteria; at least one choice is required."
                )));
            }
            Question::Score { criteria, .. } if criteria.len() < 2 => {
                return Err(Error::sdk(format!(
                    "Score question \"{name}\" has {} criteria; at least two scores are required.",
                    criteria.len()
                )));
            }
            Question::Noul {
                instructions,
                criteria,
            } if is_blank(instructions)
                && criteria
                    .as_ref()
                    .is_none_or(|c| is_blank(&c.yes) && is_blank(&c.no)) =>
            {
                return Err(Error::sdk(format!(
                    "Noul question \"{name}\" has neither instructions nor criteria; at least one is required."
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

/// A yes/no answer.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NoulAnswer {
    /// Probability of a yes answer, from zero to one.
    pub noul: f64,
}

/// A selected label and its probabilities.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}

/// An expected score with its rubric and probabilities.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScoreAnswer {
    /// Expected score, which may fall between integer rubric levels.
    pub score: f64,
    pub confidence: f64,
    /// Rubric descriptions keyed by score.
    pub legend: BTreeMap<String, Value>,
    /// Probabilities keyed by score.
    pub probabilities: BTreeMap<String, f64>,
}

/// A box's edges as fractions of the image's height (`top`, `bottom`) and width (`left`, `right`).
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct BoxCoordinates {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

/// The answer to a `box` question.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BoxAnswer {
    #[serde(rename = "box")]
    pub bounds: BoxCoordinates,
    /// `None` when the API did not report one.
    pub confidence: Option<f64>,
}

/// An answer, identified on the wire by its `type` field.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul(NoulAnswer),
    Choice(ChoiceAnswer),
    Score(ScoreAnswer),
    Box(BoxAnswer),
}

impl Answer {
    pub fn as_noul(&self) -> Option<&NoulAnswer> {
        if let Self::Noul(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn as_choice(&self) -> Option<&ChoiceAnswer> {
        if let Self::Choice(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn as_score(&self) -> Option<&ScoreAnswer> {
        if let Self::Score(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn as_box(&self) -> Option<&BoxAnswer> {
        if let Self::Box(a) = self {
            Some(a)
        } else {
            None
        }
    }
}

/// Token usage for a request; `None` when the API did not report it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Answers keyed by question name, with model and usage metadata.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SystemOneResult {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    pub usage: Usage,
}

/// Metadata for an available model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelCard {
    pub name: String,
    pub description: String,
    pub release_date: String,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// State and named questions for [`Client::system_one`](crate::Client::system_one).
#[derive(Clone, Debug)]
pub struct SystemOneRequest {
    pub(crate) state: Value,
    pub(crate) questions: BTreeMap<String, Question>,
    pub(crate) model: Option<String>,
    pub(crate) assets: BTreeMap<String, Image>,
    pub(crate) images: Vec<Image>,
}

impl SystemOneRequest {
    /// Start a request. `state` is text, a JSON object, or an array to evaluate.
    pub fn new(state: impl Into<Value>) -> Self {
        Self {
            state: state.into(),
            questions: BTreeMap::new(),
            model: None,
            assets: BTreeMap::new(),
            images: Vec::new(),
        }
    }

    /// Add a question under the name that identifies its answer.
    pub fn question(mut self, name: impl Into<String>, question: Question) -> Self {
        self.questions.insert(name.into(), question);
        self
    }

    /// Override the client's default model.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Send `image`, which the content refers to by its `Display` text (`format!("{image}")`
    /// or `Value::from(&image)`). Sent once however many times it is used.
    pub fn image(mut self, image: &Image) -> Self {
        self.images.push(image.clone());
        self
    }

    /// Send `image` under a reference of your choice, such as `<photo>`, written literally in
    /// `state` or question text. The name must appear in the request.
    pub fn asset(mut self, name: impl Into<String>, image: &Image) -> Self {
        self.assets.insert(name.into(), image.clone());
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn one(q: Question) -> BTreeMap<String, Question> {
        BTreeMap::from([("q".to_owned(), q)])
    }

    #[test]
    fn serializes_wire_shape() {
        let v = serde_json::to_value(choice("pick", [("a", Value::Null), ("b", json!("desc"))]))
            .unwrap();
        assert_eq!(
            v,
            json!({"type": "choice", "instructions": "pick", "criteria": {"a": null, "b": "desc"}})
        );
        let v = serde_json::to_value(noul("ok?")).unwrap();
        assert_eq!(v, json!({"type": "noul", "instructions": "ok?"}));
        assert_eq!(serde_json::to_value(locate("dog")).unwrap()["type"], "box");
    }

    #[test]
    fn validates() {
        assert!(validate_questions(&BTreeMap::new()).is_err());
        assert!(validate_questions(&one(score("s", ["only"]))).is_err());
        assert!(validate_questions(&one(score("s", ["lo", "hi"]))).is_ok());
        assert!(validate_questions(&one(choice("c", Vec::<(String, Value)>::new()))).is_err());
        assert!(validate_questions(&one(noul(Value::Null))).is_err());
        let criteria = NoulCriteria {
            yes: Some(json!("is x")),
            no: None,
        };
        let q = Question::Noul {
            instructions: None,
            criteria: Some(criteria),
        };
        assert!(validate_questions(&one(q)).is_ok());
    }

    #[test]
    fn deserializes_answers() {
        let r: SystemOneResult = serde_json::from_value(json!({
            "model": "jev-latest",
            "answers": {
                "a": {"type": "noul", "noul": 0.9},
                "b": {"type": "choice", "choice": "x", "confidence": 0.8, "probabilities": {"x": 0.8, "y": 0.2}},
                "c": {"type": "box", "box": {"top": 0.1, "bottom": 0.5, "left": 0.2, "right": 0.6}, "confidence": null},
            },
            "usage": {"input_tokens": 10, "output_tokens": null},
        }))
        .unwrap();
        assert_eq!(r.answers["a"].as_noul().unwrap().noul, 0.9);
        assert_eq!(r.answers["b"].as_choice().unwrap().choice, "x");
        assert!(r.answers["c"].as_box().unwrap().confidence.is_none());
        assert_eq!(r.usage.input_tokens, Some(10));
    }
}
