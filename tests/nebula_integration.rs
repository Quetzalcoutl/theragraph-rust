use theragraph::recommendation::graph_client::GraphClient;

#[tokio::test]
#[ignore]
async fn nebula_show_spaces_integration() {
    // This test is ignored by default. Run with `cargo test -- --ignored` and ensure
    // the PoC Nebula stack is running (docker-compose up -d in `Theragraph/poc`).

    // Use host/port from env or defaults suitable for the PoC
    std::env::set_var("NEBULA_HOST", std::env::var("NEBULA_HOST").unwrap_or("localhost".to_string()));
    std::env::set_var("NEBULA_PORT", std::env::var("NEBULA_PORT").unwrap_or("9669".to_string()));

    let client = GraphClient::new();

    // Retry for a short window to wait for Nebula to be ready
    let mut ok = false;
    for _ in 0..10 {
        match client.execute_query("SHOW SPACES;").await {
            Ok(output) => {
                if output.contains("theragraph") {
                    ok = true;
                    break;
                }
            }
            Err(_e) => {
                // wait and retry
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    assert!(ok, "Nebula does not report 'theragraph' space; ensure PoC is up and initialized.");
}
