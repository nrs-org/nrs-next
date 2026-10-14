//! How many works' credits `infer_music_vars` reproduces.
//! `cargo run -p nrs-import --features scripting --example credit_vars -- nrs-strip.db`
fn main() {
    let path = std::env::args().nth(1).expect("db path");
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT to_e, dah_params, transform FROM relation WHERE kind = 'credit'")
        .unwrap();
    let mut works: std::collections::BTreeMap<String, Vec<(String, nrs_core::Transform)>> =
        Default::default();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)))
        .unwrap()
    {
        let (work, params, transform) = row.unwrap();
        let roles = params
            .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
            .and_then(|v| v["roles"].as_str().map(String::from))
            .unwrap_or_default();
        works.entry(work).or_default().push((roles, serde_json::from_str(&transform).unwrap()));
    }
    let (mut ok, mut feat, mut arrange) = (0, 0, 0);
    for (work, credits) in &works {
        match nrs_import::roles::infer_music_vars(credits) {
            Some(vars) => {
                ok += 1;
                feat += vars.feat as usize;
                arrange += (vars.arrange != 0.5) as usize;
            }
            None => println!("no vars: {work} {:?}", credits.iter().map(|c| &c.0).collect::<Vec<_>>()),
        }
    }
    println!("{ok}/{} works reproduced ({feat} with feat, {arrange} with an arrange override)", works.len());
}
