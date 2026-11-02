//! Text is shaped once per unique (string, size) with Parley and stored as positioned glyph
//! runs. Renderers only replay glyph runs; nothing is shaped per frame.

use parley::{FontContext, FontData, FontFamily, LayoutContext, PositionedLayoutItem, StyleProperty};

pub struct GlyphRunData {
    pub font: FontData,
    pub size: f32,
    /// (glyph id, x, y) relative to the layout's top-left.
    pub glyphs: Vec<(u32, f32, f32)>,
}

pub struct TextLayout {
    pub text: String,
    pub size: f32,
    pub weight: f32,
    pub runs: Vec<GlyphRunData>,
    pub width: f32,
    pub height: f32,
}

pub struct Shaper {
    fcx: FontContext,
    lcx: LayoutContext<()>,
    /// CSS font-family list used for every layout.
    pub family: String,
}

impl Default for Shaper {
    fn default() -> Self {
        Self { fcx: FontContext::new(), lcx: LayoutContext::new(), family: "system-ui, sans-serif".into() }
    }
}

impl Shaper {
    /// Register font bytes (needed on WASM, where there are no system fonts). Returns family names.
    pub fn register_font(&mut self, bytes: Vec<u8>) -> Vec<String> {
        let blob = parley::fontique::Blob::new(std::sync::Arc::new(bytes));
        let fams = self.fcx.collection.register_fonts(blob, None);
        fams.iter().filter_map(|(id, _)| self.fcx.collection.family_name(*id).map(str::to_string)).collect()
    }

    pub fn shape(&mut self, text: &str, size: f32, weight: f32, max_width: Option<f32>) -> TextLayout {
        let mut b = self.lcx.ranged_builder(&mut self.fcx, text, 1.0, true);
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(self.family.clone().into())));
        b.push_default(StyleProperty::FontWeight(parley::FontWeight::new(weight)));
        let mut layout: parley::Layout<()> = b.build(text);
        layout.break_all_lines(max_width);
        layout.align(parley::Alignment::Start, parley::AlignmentOptions::default());
        let mut runs = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(gr) = item {
                    let run = gr.run();
                    runs.push(GlyphRunData {
                        font: run.font().clone(),
                        size: run.font_size(),
                        glyphs: gr.positioned_glyphs().map(|g| (g.id, g.x, g.y)).collect(),
                    });
                }
            }
        }
        TextLayout { text: text.to_string(), size, weight, runs, width: layout.width(), height: layout.height() }
    }
}
