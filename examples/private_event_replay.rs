//! 对本机已收到的真实私有回报再投递，证明去重不改变经济状态；没有网络请求。
use kaze_quant::{
    execution::ExecutionJournal,
    user_stream::{ExecutionEvent, UserEvent},
};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("Usage: private_event_replay EXISTING_TESTNET_DB")?;
    let read = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = read.prepare("SELECT body FROM private_events ORDER BY rowid LIMIT 10001")?;
    let events = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if events.is_empty() || events.len() > 10000 {
        return Err("example requires 1..=10000 captured execution events".into());
    }
    drop(stmt);
    drop(read);
    let mut journal = ExecutionJournal::open(Path::new(&path))?;
    let before = journal.audit()?;
    let mut duplicates = 0;
    for body in events {
        let event: ExecutionEvent = serde_json::from_str(&body)?;
        if journal.ingest_user_event(&UserEvent::Execution(event))? {
            return Err("captured event unexpectedly inserted".into());
        }
        duplicates += 1;
    }
    if before != journal.audit()? {
        return Err("duplicate replay changed audit/economics".into());
    }
    println!(
        "{}",
        serde_json::json!({"schema_version":1,"mode":"captured-private-execution-replay","duplicate_events":duplicates,"new_events":0,"audit_and_assets_equal":true,"scope":"local re-delivery of persisted events, no physical packet duplication or authentication-signature proof"})
    );
    Ok(())
}
