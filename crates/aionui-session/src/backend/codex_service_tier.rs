//! Verified: Codex 0.161.0 generated v2/ModelListResponse.json,
//! ThreadSettingsUpdateParams.json and ThreadSettingsUpdatedNotification.json.
use crate::capability::{ServiceTierOption, ServiceTierSnapshot};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default, Clone)]
pub(super) struct State {
    models: HashMap<String, Vec<ServiceTierOption>>,
    default_model: Option<String>,
    current_model: Option<String>,
    current_value: Option<String>,
    pub revision: u64,
}

impl State {
    pub fn discover(&mut self, result: &Value) {
        self.models.clear();
        for model in result.get("data").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = model.get("id").and_then(Value::as_str) else {
                continue;
            };
            if model.get("isDefault").and_then(Value::as_bool) == Some(true) {
                self.default_model = Some(id.to_owned());
            }
            let Some(advertised) = model.get("serviceTiers").and_then(Value::as_array) else {
                continue;
            };
            let tiers = advertised
                .iter()
                .filter_map(|tier| {
                    Some(ServiceTierOption {
                        value: tier.get("id")?.as_str()?.to_owned(),
                        name: tier.get("name")?.as_str()?.to_owned(),
                        description: tier.get("description").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect();
            self.models.insert(id.to_owned(), tiers);
        }
    }

    pub fn observe(&mut self, settings: &Value) {
        if let Some(model) = settings.get("model").and_then(Value::as_str) {
            self.current_model = Some(model.to_owned());
        }
        if let Some(tier) = settings.get("serviceTier") {
            match tier {
                Value::Null => self.current_value = Some("standard".into()),
                Value::String(value) => {
                    self.current_value = Some(if value == "default" {
                        "standard".into()
                    } else {
                        value.clone()
                    })
                }
                _ => return,
            }
            self.revision += 1;
        }
    }

    pub fn snapshot(&self) -> Option<ServiceTierSnapshot> {
        let model = self.current_model.as_ref().or(self.default_model.as_ref())?;
        let tiers = self.models.get(model)?;
        let mut options = vec![ServiceTierOption {
            value: "standard".into(),
            name: "Standard".into(),
            description: None,
        }];
        options.extend(tiers.clone());
        Some(ServiceTierSnapshot {
            current_value: self.current_value.clone(),
            options,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiers_follow_model_and_only_settings_confirm_values() {
        let mut state = State::default();
        state.discover(&serde_json::json!({"data":[
            {"id":"fast-model","isDefault":true,"serviceTiers":[{"id":"priority","name":"Fast","description":"Higher usage"}]},
            {"id":"ordinary-model","serviceTiers":[]}
        ]}));
        assert_eq!(state.snapshot().unwrap().current_value, None);
        assert_eq!(state.snapshot().unwrap().options[1].value, "priority");
        state.observe(&serde_json::json!({"model":"fast-model","serviceTier":"priority"}));
        assert_eq!(state.snapshot().unwrap().current_value.as_deref(), Some("priority"));
        state.observe(&serde_json::json!({"serviceTier":null}));
        assert_eq!(state.snapshot().unwrap().current_value.as_deref(), Some("standard"));
        state.observe(&serde_json::json!({"model":"ordinary-model"}));
        assert_eq!(state.snapshot().unwrap().options.len(), 1);
    }
}
