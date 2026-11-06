//! Valid ZIP archives holding the parts of Word, Excel, PowerPoint and
//! OpenDocument files, with fuzzed content: tests the XML readers.
#![no_main]

use libfuzzer_sys::fuzz_target;
use narwhal_fuzz::{zip, Part};

fuzz_target!(|parts: Vec<Part>| {
    let files: Vec<(&str, &[u8])> = parts.iter().map(|p| (p.name(), p.data.as_slice())).collect();
    let _ = narwhal_office::parse(&zip(&files));
});
