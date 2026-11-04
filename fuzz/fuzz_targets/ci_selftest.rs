//! Not a fuzz target: the self-test of `fuzz/ci-run.sh` and the CI jobs
//! around it (`fuzz-nightly.yml`, job `selftest`). With
//! `KASANE_FUZZ_SELFTEST` set to `crash`, `timeout` or `oom`, every input
//! panics, never returns, or grows the resident set past the 2 GiB
//! limit, so CI can check that each is reported as a failure with its
//! input saved. Unset (or anything else), it does nothing.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match std::env::var("KASANE_FUZZ_SELFTEST").as_deref() {
        Ok("crash") => panic!("ci selftest: crash"),
        Ok("timeout") => loop {
            std::hint::black_box(data);
        },
        Ok("oom") => {
            // 64 MiB every 100 ms, each byte written so it is resident,
            // in chunks under the 2 GiB malloc limit: the RSS limit
            // (checked once a second) fires about 1 GiB past 2 GiB at
            // most, far below a CI runner's memory.
            let mut held: Vec<Vec<u8>> = Vec::new();
            loop {
                held.push(vec![1u8; 64 << 20]);
                std::hint::black_box(&held);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        _ => {}
    }
});
