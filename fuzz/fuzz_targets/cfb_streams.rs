//! Valid compound files holding the streams of .doc, .xls and .ppt files,
//! with fuzzed content: tests the binary-format readers.
#![no_main]

use libfuzzer_sys::fuzz_target;
use narwhal_fuzz::{cfb, Stream};

fuzz_target!(|streams: Vec<Stream>| {
    let list: Vec<(&str, &[u8])> = streams.iter().map(|s| (s.name(), s.data.as_slice())).collect();
    if let Some(file) = cfb(&list) {
        let _ = narwhal_office::parse(&file);
    }
});
