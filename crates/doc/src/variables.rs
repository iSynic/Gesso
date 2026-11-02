//! Design tokens: collections of typed variables with one value per mode (theme).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::Id;
use crate::model::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VariableType {
    Color,
    Number,
    String,
    Boolean,
}

/// A value for one mode: a literal, or `{"alias": "<variable id>"}`.
/// Colours are written as `#rrggbb[aa]` strings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum VariableValue {
    Alias { alias: Id },
    Boolean(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Mode {
    pub id: Id,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub id: Id,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: VariableType,
    /// Mode id → value. Modes without a value fall back to the collection's default mode.
    pub values: BTreeMap<Id, VariableValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub id: Id,
    pub name: String,
    pub modes: Vec<Mode>,
    pub default_mode: Id,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Variables {
    pub collections: Vec<Collection>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    Color(Color),
    Number(f64),
    String(String),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VariableError {
    #[error("unknown variable {0}")]
    Unknown(Id),
    #[error("variable {0} has no value for mode {1} or the default mode")]
    NoValue(Id, Id),
    #[error("alias cycle through variable {0}")]
    Cycle(Id),
    #[error("variable {0}: value does not match type {1:?}")]
    TypeMismatch(Id, VariableType),
    #[error("duplicate id {0} in variables")]
    Duplicate(Id),
    #[error("collection {0}: default mode is not one of its modes")]
    BadDefaultMode(Id),
    #[error("variable {0}: value for unknown mode {1}")]
    UnknownMode(Id, Id),
}

impl Variables {
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty()
    }

    pub fn find(&self, id: Id) -> Option<(&Collection, &Variable)> {
        self.collections.iter().find_map(|c| c.variables.iter().find(|v| v.id == id).map(|v| (c, v)))
    }

    /// Resolves `id` for the chosen modes (collection id → mode id; collections not listed use
    /// their default mode), following aliases across collections.
    pub fn resolve(&self, id: Id, modes: &BTreeMap<Id, Id>) -> Result<Resolved, VariableError> {
        let mut seen = Vec::new();
        let mut cur = id;
        loop {
            if seen.contains(&cur) {
                return Err(VariableError::Cycle(cur));
            }
            seen.push(cur);
            let (coll, var) = self.find(cur).ok_or(VariableError::Unknown(cur))?;
            let mode = modes.get(&coll.id).copied().unwrap_or(coll.default_mode);
            let value = var
                .values
                .get(&mode)
                .or_else(|| var.values.get(&coll.default_mode))
                .ok_or(VariableError::NoValue(cur, mode))?;
            let mismatch = || VariableError::TypeMismatch(cur, var.ty);
            return match (value, var.ty) {
                (VariableValue::Alias { alias }, _) => {
                    cur = *alias;
                    continue;
                }
                (VariableValue::String(s), VariableType::Color) => s.parse().map(Resolved::Color).map_err(|_| mismatch()),
                (VariableValue::Number(n), VariableType::Number) => Ok(Resolved::Number(*n)),
                (VariableValue::String(s), VariableType::String) => Ok(Resolved::String(s.clone())),
                (VariableValue::Boolean(b), VariableType::Boolean) => Ok(Resolved::Boolean(*b)),
                _ => Err(mismatch()),
            };
        }
    }

    pub(crate) fn validate(&self) -> Result<(), VariableError> {
        let mut ids = std::collections::BTreeSet::new();
        for c in &self.collections {
            if !ids.insert(c.id) {
                return Err(VariableError::Duplicate(c.id));
            }
            for m in &c.modes {
                if !ids.insert(m.id) {
                    return Err(VariableError::Duplicate(m.id));
                }
            }
            if !c.modes.iter().any(|m| m.id == c.default_mode) {
                return Err(VariableError::BadDefaultMode(c.id));
            }
            for v in &c.variables {
                if !ids.insert(v.id) {
                    return Err(VariableError::Duplicate(v.id));
                }
                for (mode, value) in &v.values {
                    if !c.modes.iter().any(|m| m.id == *mode) {
                        return Err(VariableError::UnknownMode(v.id, *mode));
                    }
                    let ok = match (value, v.ty) {
                        (VariableValue::Alias { .. }, _) => true,
                        (VariableValue::String(s), VariableType::Color) => s.parse::<Color>().is_ok(),
                        (VariableValue::Number(_), VariableType::Number)
                        | (VariableValue::String(_), VariableType::String)
                        | (VariableValue::Boolean(_), VariableType::Boolean) => true,
                        _ => false,
                    };
                    if !ok {
                        return Err(VariableError::TypeMismatch(v.id, v.ty));
                    }
                }
            }
        }
        // Every alias must resolve (catches unknown targets, cycles and type mismatches).
        for c in &self.collections {
            for v in &c.variables {
                for (mode, value) in &v.values {
                    if let VariableValue::Alias { .. } = value {
                        let modes = BTreeMap::from([(c.id, *mode)]);
                        let r = self.resolve(v.id, &modes)?;
                        let ty = match r {
                            Resolved::Color(_) => VariableType::Color,
                            Resolved::Number(_) => VariableType::Number,
                            Resolved::String(_) => VariableType::String,
                            Resolved::Boolean(_) => VariableType::Boolean,
                        };
                        if ty != v.ty {
                            return Err(VariableError::TypeMismatch(v.id, v.ty));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
