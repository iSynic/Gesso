//! Every ```js block in the guides runs, each in a fresh headless document, so the examples
//! agents copy stay correct. With `GUIDE_SHOTS=DIR`, the screenshots they take are written there.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent::{Host, Request, Workspace, guides};

fn js_blocks(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(usize, String)> = None;
    for (i, line) in text.lines().enumerate() {
        match (&mut cur, line.trim_end()) {
            (None, "```js") => cur = Some((i + 1, String::new())),
            (Some(_), "```") => out.extend(cur.take()),
            (Some((_, code)), l) => {
                code.push_str(l);
                code.push('\n');
            }
            _ => {}
        }
    }
    assert!(cur.is_none(), "unclosed code block");
    out
}

#[test]
fn guide_examples_run() {
    let dir = std::env::temp_dir().join(format!("agent-guides-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shots = std::env::var("GUIDE_SHOTS").ok().map(std::path::PathBuf::from);
    let mut n = 0;
    for name in guides::NAMES {
        let text = guides::get(name).unwrap();
        for (line, code) in js_blocks(&text) {
            let mut w = Workspace::new(text::FontDb::bundled_only(), vec![dir.clone()]).unwrap();
            let r = w.handle(Request::Execute(script::Execute { code: Some(code.clone()), ..Default::default() }));
            assert!(r.is_ok(), "{name}.md line {line}: {:?}\n{code}", r.error);
            if let Some(d) = &shots {
                std::fs::create_dir_all(d).unwrap();
                for (i, img) in r.images.iter().enumerate() {
                    std::fs::write(d.join(format!("{name}-{line}-{i}.png")), agent::b64::decode(&img.png).unwrap()).unwrap();
                }
            }
            n += 1;
        }
    }
    assert!(n >= 12, "only {n} examples");
    let _ = std::fs::remove_dir_all(dir);
}
