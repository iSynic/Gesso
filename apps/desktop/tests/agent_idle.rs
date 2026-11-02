//! Agent requests to the app at rest (no scripted session, so it only draws when something
//! changed): get_state round trips and a 50-node `execute`. A measurement; run it in release
//! with `GPU_TESTS=1 cargo test --release -p desktop --test agent_idle -- --nocapture`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn idle_app_round_trips() {
    if std::env::var("GPU_TESTS").is_err() {
        eprintln!("skipped: set GPU_TESTS=1 to run");
        return;
    }
    let socket = format!("ai-design-engine-probe-{}.sock", std::process::id());
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let _child = Kill(std::process::Command::new(env!("CARGO_BIN_EXE_gesso")).args(["--agent-socket", &socket]).spawn().unwrap());
    let t = std::time::Instant::now();
    let mut c = loop {
        if let Ok(c) = agent::ipc::Client::connect(Some(&socket), "probe") {
            break c;
        }
        assert!(t.elapsed().as_secs() < 30);
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    std::thread::sleep(std::time::Duration::from_secs(2));
    for (label, gap) in [("back to back", 0u64), ("20 ms apart", 20)] {
        let mut rt = Vec::new();
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(gap));
            let t = std::time::Instant::now();
            assert!(c.call(&agent::Request::GetState(agent::Empty {})).unwrap().is_ok());
            rt.push(t.elapsed().as_secs_f64() * 1e3);
        }
        rt.sort_by(f64::total_cmp);
        eprintln!("idle app get_state {label}: p50 {:.3} ms, p95 {:.3} ms", rt[100], rt[190]);
    }
    let mut ex = Vec::new();
    for i in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let code = format!("insert(null, {{type: 'frame', name: 'P{i}', x: {x}, y: 0, width: 360, height: 600, autoLayout: {{direction: 'vertical', gap: 4, padding: 8}}, children: Array.from({{length: 49}}, () => ({{type: 'rectangle', width: 300, height: 8, fills: ['#88aacc']}}))}})", x = i * 400);
        let t = std::time::Instant::now();
        assert!(c.call(&agent::Request::Execute(script::Execute { code: Some(code), ..Default::default() })).unwrap().is_ok());
        ex.push(t.elapsed().as_secs_f64() * 1e3);
    }
    ex.sort_by(f64::total_cmp);
    eprintln!("idle app 50-node execute round trip: p50 {:.3} ms, p95 {:.3} ms", ex[15], ex[28]);
}
