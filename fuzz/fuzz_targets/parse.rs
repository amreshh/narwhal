//! Any bytes at all: the document readers must never panic or hang.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = narwhal_office::parse(data);
});
