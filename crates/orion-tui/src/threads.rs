//! Conversations: messages that answer each other (their `Message-ID`,
//! `In-Reply-To` and `References`) are grouped, the way mail clients show
//! threads. Messages without those headers are threads of their own.

use std::collections::HashMap;

use orion_core::model::Envelope;

/// Groups `indices` (into `envs`) into conversations. Each group is sorted
/// newest first; groups come in no particular order.
pub fn group(envs: &[Envelope], indices: &[usize]) -> Vec<Vec<usize>> {
    // Union-find over the given messages, joined through any shared id.
    let mut parent: Vec<usize> = (0..indices.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut owner: HashMap<&str, usize> = HashMap::new();
    for (pos, &i) in indices.iter().enumerate() {
        let e = &envs[i];
        for id in std::iter::once(e.message_id.as_str()).chain(e.parents.iter().map(String::as_str)).filter(|id| !id.is_empty()) {
            match owner.get(id) {
                Some(&other) => {
                    let (a, b) = (find(&mut parent, pos), find(&mut parent, other));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    owner.insert(id, pos);
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (pos, &i) in indices.iter().enumerate() {
        let root = find(&mut parent, pos);
        groups.entry(root).or_default().push(i);
    }
    groups
        .into_values()
        .map(|mut g| {
            g.sort_by(|&a, &b| envs[b].timestamp.cmp(&envs[a].timestamp).then(envs[b].id.cmp(&envs[a].id)));
            g
        })
        .collect()
}

/// "Ana, Luis, Bob +2": who wrote in the conversation, newest first.
pub fn participants(envs: &[Envelope], members: &[usize], max: usize) -> String {
    let mut names: Vec<String> = Vec::new();
    for &i in members {
        let e = &envs[i];
        let name = if e.from_name.trim().is_empty() { e.from_addr.split('@').next().unwrap_or("").to_string() } else { first_name(&e.from_name) };
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    let extra = names.len().saturating_sub(max);
    names.truncate(max);
    let mut s = names.join(", ");
    if extra > 0 {
        s.push_str(&format!(" +{extra}"));
    }
    s
}

/// "Ana García" → "Ana"; organisations ("Amazon.es") stay whole.
fn first_name(name: &str) -> String {
    let name = name.trim().trim_matches('"');
    match name.split_whitespace().next() {
        Some(first) if name.split_whitespace().count() > 1 && first.chars().all(char::is_alphabetic) => first.to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orion_core::model::{Flags, MessageId};

    fn env(id: u64, ts: i64, from: &str, msgid: &str, parents: &[&str]) -> Envelope {
        Envelope {
            id: MessageId(id),
            from_name: from.into(),
            from_addr: format!("{}@x.org", from.to_lowercase()),
            subject: "s".into(),
            timestamp: ts,
            date: String::new(),
            flags: Flags::default(),
            size: 1,
            has_attachments: false,
            message_id: msgid.into(),
            parents: parents.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn groups_by_references() {
        let envs = vec![
            env(1, 10, "Ana García", "a", &[]),
            env(2, 30, "Luis", "c", &["a", "b"]), // answers b, which we do not have
            env(3, 20, "Bob", "b", &["a"]),
            env(4, 25, "Eve", "x", &[]),
            env(5, 40, "Ana García", "", &[]), // no ids: alone
            env(6, 50, "Zoe", "z", &["b"]),   // joins through b
        ];
        let mut groups = group(&envs, &(0..envs.len()).collect::<Vec<_>>());
        groups.sort_by_key(|g| (g.len(), envs[g[0]].id));
        let ids: Vec<Vec<u64>> = groups.iter().map(|g| g.iter().map(|&i| envs[i].id.0).collect()).collect();
        assert_eq!(ids, vec![vec![4], vec![5], vec![6, 2, 3, 1]], "newest first inside a thread");
        let big = &groups[2];
        assert_eq!(participants(&envs, big, 3), "Zoe, Luis, Bob +1");
        // Only the given messages are grouped (a search filter).
        assert_eq!(group(&envs, &[0, 3]).len(), 2);
    }
}
