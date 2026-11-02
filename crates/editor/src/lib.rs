//! The UI-free editor core: an open document with its scene, camera, selection and scope;
//! hit-testing and selection rules; transforms, snapping and tools; commands, clipboard and
//! autosave; the layers-tree model. Input comes in as plain points and
//! modifier flags, so everything here is testable without a window.

pub mod a11y;
pub mod autosave;
pub mod camera;
pub mod clipboard;
pub mod commands;
pub mod components;
pub mod dirs;
pub mod inspect;
pub mod layers;
pub mod path_edit;
pub mod place;
pub mod select;
pub mod session;
pub mod snap;
pub mod text_edit;
pub mod tokens;
pub mod tools;
pub mod transform;
pub mod xform;

pub use camera::Camera;
pub use layers::{Layers, Row};
pub use session::{ClickMods, Error, Session, SvgImport, untitled_doc};
pub use commands::{Command, Key, KeyMods};
pub use snap::Feedback;
pub use tools::{Button, Controller, Cursor, Pointer, Tool};
pub use transform::{Grip, Handle, Mods, MoveGesture, ResizeGesture, RotateGesture, SelBox};
