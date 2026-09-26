use super::*;

    /// Test double: pre-queued responses, records calls for assertion.
    pub struct MockTransport {
        /// Each call pops the front response. Panics if empty (misconfigured test).
        pub responses: std::sync::Mutex<std::collections::VecDeque<crate::error::Result<(i32, i64)>>>,
        pub calls: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl MockTransport {
        pub fn succeeding(count: usize) -> Self {
            Self {
                responses: std::sync::Mutex::new(
                    (0..count).map(|i| Ok((0i32, i as i64))).collect(),
                ),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        pub fn failing(count: usize, msg: &str) -> Self {
            let msg = msg.to_string();
            Self {
                responses: std::sync::Mutex::new(
                    (0..count)
                        .map(|_| {
                            Err(crate::error::Error::Internal {
                                source: Some(anyhow::anyhow!("{}", msg.clone()).into()),
                            })
                        })
                        .collect(),
                ),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl SendTransport for MockTransport {
        fn deliver<'a>(
            &'a self,
            topic: &'a str,
            key: &'a str,
            payload: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<(i32, i64)>> + Send + 'a>> {
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockTransport: no more queued responses");
            self.calls.lock().unwrap().push((
                topic.to_string(),
                key.to_string(),
                payload.to_string(),
            ));
            Box::pin(async move { response })
        }
    }

    #[test]
    fn test_blockchain_event() {
        let event = BlockchainEvent::new(
            "SnapMinted",
            "0x1234567890123456789012345678901234567890",
            "snap",
            12345,
            "0xabcdef",
        )
        .with_log_index(0)
        .with_data(serde_json::json!({"token_id": 1}));

        assert_eq!(event.event_type, "SnapMinted");
        assert_eq!(event.block_number, 12345);
        assert!(event.data.is_some());
    }

    #[test]
    fn test_producer_stats() {
        let metrics = KafkaProducerMetrics::new();
        metrics.messages_sent.fetch_add(10, Ordering::Relaxed);
        metrics.messages_failed.fetch_add(1, Ordering::Relaxed);

        assert_eq!(metrics.messages_sent.load(Ordering::Relaxed), 10);
        assert_eq!(metrics.messages_failed.load(Ordering::Relaxed), 1);
    }
