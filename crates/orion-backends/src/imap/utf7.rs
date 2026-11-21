//! IMAP modified UTF-7 (RFC 3501 §5.1.3) for mailbox names.

/// Decodes a mailbox name; if it is malformed it is returned unchanged.
pub fn decode(name: &str) -> String {
    try_decode(name).unwrap_or_else(|| name.to_string())
}

fn try_decode(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let end = after.find('-')?;
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let bytes = decode_b64(chunk)?;
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            out.push_str(&String::from_utf16(&units).ok()?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b',' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes() {
        assert_eq!(decode("INBOX"), "INBOX");
        assert_eq!(decode("Env&AO0-o"), "Envío");
        assert_eq!(decode("Drafts &AOk-t&AOk-"), "Drafts été");
        assert_eq!(decode("Trash &- recycling"), "Trash & recycling");
        assert_eq!(decode("&ZeVnLIqe-"), "日本語");
        assert_eq!(decode("Bad&formed"), "Bad&formed");
    }
}
