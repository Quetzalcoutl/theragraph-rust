use anyhow::Result;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::time::Duration;
use tempfile::NamedTempFile;

#[derive(Debug, Deserialize)]
struct Event {
    event_id: String,
    #[serde(rename = "type")]
    event_type: String,
    actor_id: String,
    target_id: Option<String>,
    timestamp: i64,
    partition_id: i32,
}

fn group_by_partition(events: Vec<Event>) -> HashMap<i32, Vec<Event>> {
    let mut grouped: HashMap<i32, Vec<Event>> = HashMap::new();
    for e in events.into_iter() {
        grouped.entry(e.partition_id).or_default().push(e);
    }
    grouped
}

fn dedupe_events(events: Vec<Event>) -> Vec<Event> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for e in events.into_iter() {
        if seen.insert(e.event_id.clone()) {
            out.push(e);
        }
    }
    out
}

fn build_ngql_for_event(e: &Event) -> Option<String> {
    match e.event_type.as_str() {
        "follow" => {
            let a = &e.actor_id;
            let b = e.target_id.as_ref()?;
            // INSERT EDGE will overwrite existing edge (idempotent)
            let ngql = format!(
                "INSERT EDGE follows(event_id, followed_at, weight, type) VALUES \"{a}\" -> \"{b}\":(\"{eid}\", {ts}, 1.0, \"follow\");",
                a = a,
                b = b,
                eid = e.event_id,
                ts = e.timestamp
            );
            Some(ngql)
        }
        "view" => {
            let vid = e.target_id.as_ref()?;
            let ngql = format!(
                "INSERT EDGE view_event(event_id, `timestamp`) VALUES \"{a}\" -> \"{b}\":(\"{eid}\", {ts});",
                a = e.actor_id,
                b = vid,
                eid = e.event_id,
                ts = e.timestamp
            );
            Some(ngql)
        }
        "like" => {
            let a = &e.actor_id;
            let b = e.target_id.as_ref()?;
            let ngql = format!(
                "INSERT EDGE likes(event_id, liked_at, reaction_type) VALUES \"{a}\" -> \"{b}\":(\"{eid}\", {ts}, \"like\");",
                a = a,
                b = b,
                eid = e.event_id,
                ts = e.timestamp
            );
            Some(ngql)
        }
        "comment" => {
            let a = &e.actor_id;
            let b = e.target_id.as_ref()?;
            let ngql = format!(
                "INSERT EDGE comments_on(event_id, comment_text, commented_at) VALUES \"{a}\" -> \"{b}\":(\"{eid}\", \"\", {ts});",
                a = a,
                b = b,
                eid = e.event_id,
                ts = e.timestamp
            );
            Some(ngql)
        }
        _ => None,
    }
}

fn build_batch_ngql(events: &[Event]) -> String {
    let mut stmts = vec!["USE theragraph;".to_string()];
    for e in events.iter() {
        if let Some(s) = build_ngql_for_event(e) {
            stmts.push(s);
        }
    }
    stmts.join("\n")
}

fn exec_ngql_via_console(ngql: &str) -> Result<()> {
    // Use local nebula-console binary installed in the container
    // Get connection details from environment variables
    let nebula_host = std::env::var("NEBULA_HOST").unwrap_or_else(|_| "graphd".to_string());
    let nebula_port = std::env::var("NEBULA_PORT").unwrap_or_else(|_| "9669".to_string());

    // Write NGQL to a temp file to avoid shell interpolation issues
    let mut tmp = NamedTempFile::new()?;
    write!(tmp, "{}", ngql)?;
    tmp.flush()?;
    let tmp_path = tmp
        .path()
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid temp path"))?;

    println!(
        "Executing NGQL via nebula-console (host={}, port={})",
        nebula_host, nebula_port
    );

    // Execute nebula-console with the temp file as input
    let status = Command::new("nebula-console")
        .arg("-addr")
        .arg(&nebula_host)
        .arg("-port")
        .arg(&nebula_port)
        .arg("-u")
        .arg("root")
        .arg("-p")
        .arg("nebula")
        .arg("-f")
        .arg(tmp_path)
        .status()?;

    if !status.success() {
        anyhow::bail!(
            "nebula-console command failed with status: {:?}",
            status.code()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let events_file = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("examples/sample-events.jsonl");

    let f = File::open(events_file)?;
    let reader = BufReader::new(f);
    let mut events = vec![];
    for line in reader.lines() {
        let l = line?;
        if l.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(&l) {
            Ok(ev) => events.push(ev),
            Err(e) => println!("warn: failed to parse event: {} (line: {})", e, l),
        }
    }

    let grouped = group_by_partition(events);
    for (partition, evs) in grouped.into_iter() {
        println!("Partition {}: {} events", partition, evs.len());
        let dedup = dedupe_events(evs);
        let batch_ngql = build_batch_ngql(&dedup);
        if batch_ngql.trim().is_empty() {
            println!("No NGQL statements to execute for this batch");
            continue;
        }
        // Execute via nebula console
        match exec_ngql_via_console(&batch_ngql) {
            Ok(_) => println!("Batch executed for partition {}", partition),
            Err(e) => println!("Error executing batch for partition {}: {}", partition, e),
        }
        // simulate periodic flush interval
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(())
}
