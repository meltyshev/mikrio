#![no_main]

use libfuzzer_sys::fuzz_target;
use s3s::post_policy::PostPolicy;

fuzz_target!(|input: &[u8]| {
    if input.len() > 20 * 1024 {
        return;
    }

    let Ok(text) = std::str::from_utf8(input) else {
        return;
    };

    // Exercise hostile base64 independently, then compare both public decoding
    // paths on a canonical encoding of the same possibly malformed JSON.
    drop(PostPolicy::from_base64(text));

    let json_result = PostPolicy::from_json(text);
    let encoded = base64_simd::STANDARD.encode_to_string(input);
    let base64_result = PostPolicy::from_base64(&encoded);

    match (json_result, base64_result) {
        (Ok(json_policy), Ok(base64_policy)) => {
            assert_eq!(json_policy.conditions, base64_policy.conditions);
            assert_eq!(json_policy.expiration, base64_policy.expiration);
        }
        (Err(_), Err(_)) => {}
        #[expect(
            clippy::panic,
            reason = "This fuzz oracle must panic when decoding paths disagree on acceptance."
        )]
        _ => panic!("canonical base64 changed policy acceptance"),
    }
});
