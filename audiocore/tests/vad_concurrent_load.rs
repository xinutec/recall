//! Its own binary, so the shared session starts empty: the first loads race to
//! build it, as the integration tests' scanners do in one process.

/// Arrivals 5 ms apart over 95 ms: a load that comes after the first has made
/// ONNX Runtime's one environment but before its session is stored (about 38
/// to 60 ms into a debug build here) finds the environment taken. Released
/// together, every load passes that check before any environment exists, and
/// none fails.
#[test]
fn loads_that_arrive_while_the_session_is_built_all_succeed() {
    let threads: Vec<_> = (0..20_u64)
        .map(|n| {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(5 * n));
                audiocore::vad::Detector::load()
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
        })
        .collect();
    for t in threads {
        assert_eq!(t.join().expect("thread"), Ok(()));
    }
}
