//! Autocrypt (autocrypt.org Level 1) headers: learning a sender's key from
//! the mail itself, and offering ours the same way.

/// The key in an `Autocrypt:` header, if its `addr` is the sender's.
pub fn parse(raw_headers: &str, from_addr: &str) -> Option<Vec<u8>> {
    // Unfold continuation lines.
    let mut headers: Vec<String> = Vec::new();
    for line in raw_headers.lines() {
        if line.starts_with([' ', '\t'])
            && let Some(last) = headers.last_mut() {
                last.push_str(line.trim());
                continue;
            }
        headers.push(line.to_string());
    }
    for h in headers {
        let Some((name, value)) = h.split_once(':') else { continue };
        if !name.trim().eq_ignore_ascii_case("autocrypt") {
            continue;
        }
        let (mut addr, mut keydata, mut unknown_critical) = (None, None, false);
        for attr in value.split(';') {
            let Some((k, v)) = attr.split_once('=') else { continue };
            match k.trim() {
                "addr" => addr = Some(v.trim().to_string()),
                "keydata" => keydata = Some(v.chars().filter(|c| !c.is_whitespace()).collect::<String>()),
                "prefer-encrypt" => {}
                // Attributes starting with '_' are optional; any other unknown one means "ignore this header".
                other if !other.starts_with('_') => unknown_critical = true,
                _ => {}
            }
        }
        if unknown_critical {
            continue;
        }
        if let (Some(a), Some(k)) = (addr, keydata)
            && a.eq_ignore_ascii_case(from_addr) {
                return base64_decode(&k);
            }
    }
    None
}

/// Header value for our own key (`addr=…; prefer-encrypt=mutual; keydata=…`),
/// folded so no line is too long.
pub fn header_value(addr: &str, key: &[u8]) -> String {
    let data = base64_encode(key);
    let mut folded = String::with_capacity(data.len() + data.len() / 70 * 3);
    for (i, chunk) in data.as_bytes().chunks(72).enumerate() {
        if i > 0 {
            folded.push_str("\r\n ");
        }
        folded.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    format!("addr={addr}; prefer-encrypt=mutual; keydata=\r\n {folded}")
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.trim_end_matches('=').as_bytes() {
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
    use super::*;

    #[test]
    fn roundtrip_and_rules() {
        let key = (0u8..=255).collect::<Vec<u8>>();
        let value = header_value("ana@example.org", &key);
        assert!(value.lines().all(|l| l.len() <= 78), "folded");
        let headers = format!("From: Ana <ana@example.org>\nAutocrypt: {}\nSubject: x\n", value.replace("\r\n", "\n"));
        assert_eq!(parse(&headers, "ANA@example.org"), Some(key.clone()));
        // addr must be the sender's.
        assert_eq!(parse(&headers, "mallory@example.org"), None);
        // Unknown critical attribute → ignored; optional "_x" attributes are fine.
        let bad = headers.replace("prefer-encrypt=mutual", "evil=1");
        assert_eq!(parse(&bad, "ana@example.org"), None);
        let ok = headers.replace("prefer-encrypt=mutual", "_x=1");
        assert_eq!(parse(&ok, "ana@example.org"), Some(key));
    }
}
