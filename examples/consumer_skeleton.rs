use std::collections::{HashMap, HashSet};
use std::time::Duration;

// This is a simple in-memory consumer skeleton to simulate batch processing
// of events and creation of a consolidated mutation payload to send to the
// graph DB (here we print to stdout instead of sending to a real DB).

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct Event {
    event_id: String,
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
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = vec![];
    for e in events.into_iter() {
        if !seen.contains(&e.event_id) {
            seen.insert(e.event_id.clone());
            out.push(e);
        }
    }
    out
}

fn main() {
    // Sample events
    let events = vec![
        Event {
            event_id: "e1".into(),
            event_type: "follow".into(),
            actor_id: "user:1".into(),
            target_id: Some("user:2".into()),
            timestamp: 1710000000,
            partition_id: 1,
        },
        Event {
            event_id: "e2".into(),
            event_type: "follow".into(),
            actor_id: "user:1".into(),
            target_id: Some("user:3".into()),
            timestamp: 1710000004,
            partition_id: 1,
        },
        Event {
            event_id: "e3".into(),
            event_type: "view".into(),
            actor_id: "user:2".into(),
            target_id: Some("video:1".into()),
            timestamp: 1710000123,
            partition_id: 2,
        },
    ];

    let grouped = group_by_partition(events);
    for (partition, evs) in grouped.into_iter() {
        let dedup = dedupe_events(evs);
        println!("Partition {}: deduped batch: {:#?}", partition, dedup);
        // Transform into a DB mutation: here we just print a pseudo-mutation
        for e in dedup.iter() {
            match e.event_type.as_str() {
                "follow" => println!(
                    "MUTATION: add follow {} -> {}",
                    e.actor_id,
                    e.target_id.as_ref().unwrap()
                ),
                "view" => println!(
                    "MUTATION: increment view for {} -> {}",
                    e.actor_id,
                    e.target_id.as_ref().unwrap()
                ),
                _ => println!("MUTATION: unhandled event {:?}", e),
            }
        }
    }

    // Simulate periodic flush interval
    std::thread::sleep(Duration::from_millis(200));
    println!("Flush complete")
}
