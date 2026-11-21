//! Diagnostic: feeds a captured IMAP response stream to imap-proto, response by
//! response, and reports where parsing fails or stalls. Prints no mail content
//! (letters are masked).
use async_imap::imap_proto::Response;

fn mask(b: &[u8]) -> String {
    b.iter().map(|&c| if c.is_ascii_alphabetic() { 'x' } else if c.is_ascii_digit() { '9' } else if c.is_ascii_graphic() || c == b' ' { c as char } else { '·' }).collect()
}

fn main() {
    let data = std::fs::read(std::env::args().nth(1).expect("file")).unwrap();
    let mut rest = &data[..];
    let (mut ok, mut seq) = (0, 0u32);
    while !rest.is_empty() {
        match Response::parse(rest) {
            Ok((next, resp)) => {
                if let Response::Fetch(n, _) = resp { seq = n; }
                ok += 1;
                rest = next;
            }
            Err(e) => {
                let kind = match e { nom::Err::Incomplete(n) => format!("Incomplete({n:?}) — would wait forever for more data"), nom::Err::Error(x) | nom::Err::Failure(x) => format!("{:?}", x.code) };
                let line_end = rest.iter().position(|&c| c == b'\n').map(|p| p + 1).unwrap_or(rest.len());
                println!("parsed {ok} responses (last FETCH seq {seq}); then FAILED: {kind}");
                println!("offending response starts (masked): {}", mask(&rest[..line_end.min(200)]));
                // Show the structure around the failure: find where inside the response it breaks.
                return;
            }
        }
    }
    println!("all {ok} responses parsed fine");
}
