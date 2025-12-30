use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use tokio::time::sleep;

#[allow(dead_code)]
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

async fn send_dgraph_mutation(client: &Client, url: &str, mutation: &str) -> Result<()> {
    // Simple retry loop
    for i in 0..3 {
        let res = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(mutation.to_string())
            .send()
            .await;

        match res {
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                println!("Dgraph response (status={}): {}", status, body);
                if status.is_success() {
                    return Ok(());
                }
            }
            Err(e) => println!("HTTP error: {}", e),
        }
        sleep(Duration::from_millis(200 * (i + 1))).await;
    }
    Err(anyhow::anyhow!("Failed to send mutation after retries"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let events_file = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("examples/sample-events.jsonl");
    let dgraph_url = args
        .get(2)
        .map(|s| s.as_str())
        .unwrap_or("http://localhost:8080/graphql");

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
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

    for (partition, evs) in grouped.into_iter() {
        println!(
            "Processing partition {} with {} events",
            partition,
            evs.len()
        );
        let dedup = dedupe_events(evs);
        // Build a simple Dgraph mutation: create follow edges and increment views
        // For demonstration we create JSON mutations using Dgraph GraphQL upsert-like syntax.
        for e in dedup.iter() {
            match e.event_type.as_str() {
                "follow" => {
                    let a = &e.actor_id;
                    let b = e.target_id.as_deref().unwrap_or("");
                    let mutation = format!(
                        r#"{{"query":"mutation {{ updateUser(input: [{{filter: {{id: {{eq: \"{actor}\"}}}}, set: {{follows: [{{id: \"{target}\"}}]}}}}]) {{ user {{ id }} }} }}" }}"#,
                        actor = a,
                        target = b
                    );
                    let _ = send_dgraph_mutation(&client, dgraph_url, &mutation).await;
                }
                "view" => {
                    let vid = e.target_id.as_deref().unwrap_or("");
                    // In a real setup we'd use a bulk mutation / upsert to increment counter safely.
                    let mutation = format!(
                        r#"{{"query":"mutation {{ updateVideo(input: [{{filter: {{id: {{eq: \"{vid}\"}}}}, set: {{views: 1}}}}]) {{ video {{ id }} }} }}" }}"#,
                        vid = vid
                    );
                    let _ = send_dgraph_mutation(&client, dgraph_url, &mutation).await;
                }
                _ => println!("Unhandled event type: {}", e.event_type),
            }
        }
    }

    println!("All partitions processed. Sleeping briefly before exit.");
    sleep(Duration::from_millis(200)).await;
    Ok(())
}
