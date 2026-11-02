//! Accessible names for panel controls whose visible text doesn't say what they do: symbol
//! buttons, bare checkboxes, colour swatches, number fields and drop-downs in a labelled row.

pub trait Named {
    /// Sets the control's name for screen readers.
    fn named(self, label: impl Into<String>) -> Self;
    /// A tooltip that is also the control's accessible name.
    fn tip(self, text: impl Into<String>) -> Self;
}

impl Named for egui::Response {
    fn named(self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.ctx.accesskit_node_builder(self.id, |n| n.set_label(label));
        self
    }

    fn tip(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.named(text.clone()).on_hover_text(text)
    }
}
