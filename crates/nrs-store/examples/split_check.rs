//! Infers split trees for every shared impact in a database and checks
//! they reproduce the stored shares. `cargo run -p nrs-store --example
//! split_check -- <db>`
use nrs_core::ImpactId;
use nrs_store::{flatten_split, infer_split, SplitNode, SplitRule, Store};

fn shape(node: &SplitNode) -> String {
    match node {
        SplitNode::Member { .. } => "m".into(),
        SplitNode::Group { rule, children, .. } => {
            let r = match rule { SplitRule::Explicit => "X", SplitRule::Even => "E", SplitRule::Weighted => "W" };
            let members = children.iter().filter(|c| matches!(c, SplitNode::Member { .. })).count();
            let groups: Vec<String> = children.iter().filter(|c| matches!(c, SplitNode::Group { .. })).map(shape).collect();
            format!("{r}[{members}m{}]", if groups.is_empty() { String::new() } else { format!(" {}", groups.join(" ")) })
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("db path");
    let store = Store::open(&path).unwrap();
    let conn = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let ids: Vec<String> = conn.prepare("SELECT DISTINCT impact FROM impact_member").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    let (mut worst, mut shapes) = (0f32, std::collections::BTreeMap::<String, usize>::new());
    for id in &ids {
        let members = store.impact_members(&ImpactId(id.clone())).unwrap();
        let tree = infer_split(&members);
        let flat = flatten_split(&tree);
        assert_eq!(flat.len(), members.len(), "{id}");
        for b in &members {
            let a = flat.iter().find(|a| a.contributor == b.contributor).unwrap_or_else(|| panic!("{id}: lost {}", b.contributor.0));
            worst = worst.max((a.factor - b.factor).abs());
        }
        *shapes.entry(shape(&tree)).or_default() += 1;
        if members.len() >= 20 { println!("{id} ({} members): {}", members.len(), shape(&tree)); }
    }
    println!("{} shared impacts; worst share difference after infer+flatten: {worst:e}", ids.len());
    for (s, n) in shapes.iter().filter(|(_, n)| **n >= 2) { println!("  {n:3} x {s}"); }
}
