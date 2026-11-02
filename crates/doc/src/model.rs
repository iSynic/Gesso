//! Property value types shared by nodes, the file format and the ops.
//! JSON field names are camelCase; values equal to their default are omitted on save.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::id::Id;

pub(crate) fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}
pub(crate) fn is_true(v: &bool) -> bool {
    *v
}
pub(crate) fn is_one(v: &f64) -> bool {
    *v == 1.0
}
pub(crate) fn yes() -> bool {
    true
}
pub(crate) fn one() -> f64 {
    1.0
}

// ---- colour ---------------------------------------------------------------------------------

/// sRGB colour with 8-bit channels, written as `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if self.a != 255 {
            write!(f, "{:02x}", self.a)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Color({self})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid colour {0:?}: expected #rrggbb or #rrggbbaa")]
pub struct ColorParseError(pub String);

impl FromStr for Color {
    type Err = ColorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ColorParseError(s.to_string());
        let hex = s.strip_prefix('#').ok_or_else(err)?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
            return Err(err());
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| err());
        Ok(Color { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: if hex.len() == 8 { byte(6)? } else { 255 } })
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Color {
    fn schema_name() -> Cow<'static, str> {
        "Color".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^#([0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$",
            "description": "sRGB colour #rrggbb or #rrggbbaa"
        })
    }
}

// ---- assets ---------------------------------------------------------------------------------

/// Content address of an asset: lowercase hex SHA-256 of its bytes.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct AssetId(pub String);

impl AssetId {
    pub fn is_valid(&self) -> bool {
        self.0.len() == 64 && self.0.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

impl fmt::Debug for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetId({})", self.0)
    }
}

impl JsonSchema for AssetId {
    fn schema_name() -> Cow<'static, str> {
        "AssetId".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "pattern": "^[0-9a-f]{64}$", "description": "SHA-256 of the asset bytes" })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetMeta {
    /// MIME type, e.g. `image/png`.
    pub mime: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub width: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub height: u32,
}

// ---- geometry-ish enums ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Sizing {
    #[default]
    Fixed,
    Hug,
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintH {
    #[default]
    Left,
    Right,
    LeftRight,
    Center,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintV {
    #[default]
    Top,
    Bottom,
    TopBottom,
    Center,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Constraints {
    #[serde(default, skip_serializing_if = "is_default")]
    pub h: ConstraintH,
    #[serde(default, skip_serializing_if = "is_default")]
    pub v: ConstraintV,
}

/// Corner radius: one number for all corners, or `[topLeft, topRight, bottomRight, bottomLeft]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Radius {
    Uniform(f64),
    Each([f64; 4]),
}

impl Default for Radius {
    fn default() -> Self {
        Radius::Uniform(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    ColorBurn,
    Lighten,
    Screen,
    ColorDodge,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

// ---- paints ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GradientStop {
    /// 0..=1 along the gradient.
    pub offset: f64,
    pub color: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    #[default]
    Fill,
    Fit,
    Crop,
    Tile,
}

/// What a paint draws. Gradient points are in the node's unit square (0..1, 0..1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PaintKind {
    Solid {
        color: Color,
    },
    #[serde(rename_all = "camelCase")]
    LinearGradient {
        stops: Vec<GradientStop>,
        start: [f64; 2],
        end: [f64; 2],
    },
    #[serde(rename_all = "camelCase")]
    RadialGradient {
        stops: Vec<GradientStop>,
        center: [f64; 2],
        /// Radii along x and y, in the unit square.
        radius: [f64; 2],
    },
    #[serde(rename_all = "camelCase")]
    AngularGradient {
        stops: Vec<GradientStop>,
        center: [f64; 2],
        /// Start angle in degrees.
        #[serde(default)]
        angle: f64,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        asset: AssetId,
        #[serde(default, skip_serializing_if = "is_default")]
        fit: ImageFit,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Paint {
    #[serde(flatten)]
    pub kind: PaintKind,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f64,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub blend: BlendMode,
}

impl Paint {
    pub fn solid(color: Color) -> Self {
        Paint { kind: PaintKind::Solid { color }, opacity: 1.0, visible: true, blend: BlendMode::Normal }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StrokeAlign {
    #[default]
    Inside,
    Center,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StrokeCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StrokeJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub paint: Paint,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub weight: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub align: StrokeAlign,
    /// Dash pattern lengths; empty means solid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dash: Vec<f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub cap: StrokeCap,
    #[serde(default, skip_serializing_if = "is_default")]
    pub join: StrokeJoin,
}

// ---- effects --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Shadow {
    pub color: Color,
    #[serde(default)]
    pub offset: [f64; 2],
    #[serde(default)]
    pub blur: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub spread: f64,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Effect {
    DropShadow(Shadow),
    InnerShadow(Shadow),
    #[serde(rename_all = "camelCase")]
    LayerBlur {
        radius: f64,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        visible: bool,
    },
    #[serde(rename_all = "camelCase")]
    BackgroundBlur {
        radius: f64,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        visible: bool,
    },
}

// ---- auto-layout ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LayoutDirection {
    #[default]
    Horizontal,
    Vertical,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MainAlign {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CrossAlign {
    #[default]
    Start,
    Center,
    End,
    Stretch,
    Baseline,
}

/// A grid track size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Track {
    Fixed(f64),
    /// Fraction of the remaining space.
    Fr(f64),
    Auto,
}

/// Padding `[top, right, bottom, left]`.
pub type Padding = [f64; 4];

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutoLayout {
    #[serde(default, skip_serializing_if = "is_default")]
    pub direction: LayoutDirection,
    #[serde(default, skip_serializing_if = "is_default")]
    pub gap: f64,
    /// Gap between wrapped lines or grid rows.
    #[serde(default, skip_serializing_if = "is_default")]
    pub cross_gap: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub padding: Padding,
    #[serde(default, skip_serializing_if = "is_default")]
    pub main_align: MainAlign,
    #[serde(default, skip_serializing_if = "is_default")]
    pub cross_align: CrossAlign,
    #[serde(default, skip_serializing_if = "is_default")]
    pub wrap: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Track>,
}

/// How a node behaves as a child of an auto-layout frame.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutChild {
    /// Ignore auto-layout and position by x/y.
    #[serde(default, skip_serializing_if = "is_default")]
    pub absolute: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub grow: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_self: Option<CrossAlign>,
    /// Grid placement `[column, row]`, 0-based; `None` = auto-placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_cell: Option<[u32; 2]>,
}

// ---- text -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TextDecoration {
    #[default]
    None,
    Underline,
    Strikethrough,
}

/// Full text style (used as a text node's base style).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    pub font_family: String,
    pub font_size: f64,
    #[serde(default = "default_weight", skip_serializing_if = "is_regular")]
    pub font_weight: u16,
    #[serde(default, skip_serializing_if = "is_default")]
    pub italic: bool,
    /// Line height in px; `None` = font default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub letter_spacing: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub decoration: TextDecoration,
    pub color: Color,
}

fn default_weight() -> u16 {
    400
}
fn is_regular(w: &u16) -> bool {
    *w == 400
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            font_family: "Inter".into(),
            font_size: 14.0,
            font_weight: 400,
            italic: false,
            line_height: None,
            letter_spacing: 0.0,
            decoration: TextDecoration::None,
            color: Color::BLACK,
        }
    }
}

/// A run of text with optional overrides of the node's base style.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextSpan {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoration: Option<TextDecoration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// Hyperlink target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VerticalAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// How a text box sizes itself from its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TextAutoResize {
    /// Fixed box; text may overflow.
    None,
    /// Fixed width, height grows.
    #[default]
    Height,
    /// Single line grows in both directions.
    WidthAndHeight,
}

// ---- paths ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

/// One SVG-syntax path (`d` attribute), in the node's local coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PathData {
    pub d: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fill_rule: FillRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Arc {
    /// Start angle, degrees.
    pub start: f64,
    /// Sweep, degrees.
    pub sweep: f64,
    /// Inner radius as a fraction of the outer (donut), 0..1.
    #[serde(default)]
    pub inner_ratio: f64,
}

/// Id type alias used where an id names a component's descendant (override targets).
pub type TargetId = Id;
