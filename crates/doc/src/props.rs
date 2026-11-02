//! Properties shared by every node kind, plus the `PropKey`/`PropValue` pair used by update ops.
//! One macro keeps the struct, the key enum and the value enum in sync.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::Id;
use crate::model::*;
use crate::node::NodeKind;

macro_rules! common_props {
    ($( $(#[$m:meta])* $field:ident : $ty:ty => $Variant:ident ),* $(,)?) => {
        /// Properties every node has. Missing fields take their defaults.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "camelCase")]
        pub struct CommonProps {
            $( $(#[$m])* pub $field: $ty, )*
        }

        /// Names one updatable property. `Name` and `Kind` live on the node itself; `Kind` is the
        /// kind-specific payload (e.g. a text node's spans) and cannot change the node's type.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "camelCase")]
        pub enum PropKey {
            Name,
            Kind,
            $( $Variant, )*
        }

        /// A new value for one property.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(tag = "prop", content = "value", rename_all = "camelCase")]
        pub enum PropValue {
            Name(String),
            Kind(NodeKind),
            $( $Variant($ty), )*
        }

        impl PropKey {
            pub const COMMON: &'static [PropKey] = &[$( PropKey::$Variant, )*];
        }

        impl PropValue {
            pub fn key(&self) -> PropKey {
                match self {
                    PropValue::Name(_) => PropKey::Name,
                    PropValue::Kind(_) => PropKey::Kind,
                    $( PropValue::$Variant(_) => PropKey::$Variant, )*
                }
            }
        }

        impl CommonProps {
            /// Appends the keys whose values differ between `self` and `other`.
            pub fn diff(&self, other: &CommonProps, out: &mut Vec<PropKey>) {
                $( if self.$field != other.$field { out.push(PropKey::$Variant); } )*
            }

            pub(crate) fn get(&self, key: PropKey) -> Option<PropValue> {
                match key {
                    $( PropKey::$Variant => Some(PropValue::$Variant(self.$field.clone())), )*
                    PropKey::Name | PropKey::Kind => None,
                }
            }

            /// Stores `value` and returns the previous value; hands back `Name`/`Kind` unchanged.
            #[allow(clippy::result_large_err, reason = "the Err value is the caller's own input, returned unmoved")]
            pub(crate) fn set(&mut self, value: PropValue) -> Result<PropValue, PropValue> {
                match value {
                    $( PropValue::$Variant(v) => Ok(PropValue::$Variant(std::mem::replace(&mut self.$field, v))), )*
                    other => Err(other),
                }
            }
        }
    };
}

common_props! {
    #[serde(default, skip_serializing_if = "is_default")]
    x: f64 => X,
    #[serde(default, skip_serializing_if = "is_default")]
    y: f64 => Y,
    /// Degrees, clockwise, around the node's centre.
    #[serde(default, skip_serializing_if = "is_default")]
    rotation: f64 => Rotation,
    #[serde(default, skip_serializing_if = "is_default")]
    flip_x: bool => FlipX,
    #[serde(default, skip_serializing_if = "is_default")]
    flip_y: bool => FlipY,
    #[serde(default)]
    width: f64 => Width,
    #[serde(default)]
    height: f64 => Height,
    #[serde(default, skip_serializing_if = "is_default")]
    sizing_h: Sizing => SizingH,
    #[serde(default, skip_serializing_if = "is_default")]
    sizing_v: Sizing => SizingV,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_width: Option<f64> => MinWidth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_width: Option<f64> => MaxWidth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_height: Option<f64> => MinHeight,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_height: Option<f64> => MaxHeight,
    #[serde(default, skip_serializing_if = "is_default")]
    constraints: Constraints => Constraints,
    #[serde(default, skip_serializing_if = "is_default")]
    radius: Radius => Radius,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fills: Vec<Paint> => Fills,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    strokes: Vec<Stroke> => Strokes,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect> => Effects,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    opacity: f64 => Opacity,
    #[serde(default, skip_serializing_if = "is_default")]
    blend: BlendMode => Blend,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    visible: bool => Visible,
    #[serde(default, skip_serializing_if = "is_default")]
    locked: bool => Locked,
    #[serde(default, skip_serializing_if = "is_default")]
    layout_child: LayoutChild => LayoutChild,
    /// Design-token bindings: property path (e.g. `fills/0/color`, `opacity`) → variable id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    bindings: BTreeMap<String, Id> => Bindings,
    /// Variable modes for this node and its content: collection id → mode id. Only pages,
    /// frames, components and instances take it; others inherit from their ancestors.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    modes: BTreeMap<Id, Id> => Modes,
}

impl Default for CommonProps {
    fn default() -> Self {
        CommonProps {
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            flip_x: false,
            flip_y: false,
            width: 0.0,
            height: 0.0,
            sizing_h: Sizing::Fixed,
            sizing_v: Sizing::Fixed,
            min_width: None,
            max_width: None,
            min_height: None,
            max_height: None,
            constraints: Constraints::default(),
            radius: Radius::default(),
            fills: Vec::new(),
            strokes: Vec::new(),
            effects: Vec::new(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            visible: true,
            locked: false,
            layout_child: LayoutChild::default(),
            bindings: BTreeMap::new(),
            modes: BTreeMap::new(),
        }
    }
}

impl PropKey {
    /// The JSON field name, e.g. `fills`, `sizingH`.
    pub fn as_str(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(s)) => s,
            _ => String::new(),
        }
    }

    pub fn parse(name: &str) -> Option<PropKey> {
        serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
    }
}

impl PropValue {
    /// Builds a value from a JSON field name and value, e.g. (`"opacity"`, `0.5`).
    pub fn from_json(key: PropKey, value: serde_json::Value) -> Result<PropValue, serde_json::Error> {
        serde_json::from_value(serde_json::json!({ "prop": key, "value": value }))
    }
}
