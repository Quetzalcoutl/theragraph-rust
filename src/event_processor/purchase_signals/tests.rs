use super::*;

fn row(log_index: Option<i32>) -> PurchaseRow {
    PurchaseRow {
        id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        inserted_at: NaiveDateTime::parse_from_str("2026-09-26 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap(),
        buyer_address: "0xBUYER".into(),
        tx_hash: "0xabc".into(),
        block_number: 42,
        log_index,
        nft_uuid: Uuid::nil(),
        contract_address: "0xMARKET".into(),
        contract_type: Some("art".into()),
    }
}

#[test]
fn v2_event_id_is_keyed_by_log_index() {
    let e = event_for(&row(Some(7)));
    assert_eq!(e.event_type, "Purchase:7");
    assert_eq!(e.log_index, 7);
    assert_eq!(e.transaction_hash, "0xabc");
    assert_eq!(e.contract_address, "0xmarket");
    assert_eq!(e.contract_type, "art");
    assert_eq!(e.block_number, 42);
}

#[test]
fn v1_event_id_falls_back_to_purchase_id() {
    let e = event_for(&row(None));
    assert_eq!(e.event_type, "Purchase:11111111-2222-3333-4444-555555555555");
    assert_eq!(e.log_index, 0);
}

#[test]
fn two_sales_in_one_tx_get_distinct_event_ids() {
    // enrich_and_record_pools builds event_id = "{tx}:{event_type}".
    let a = event_for(&row(Some(3)));
    let b = event_for(&row(Some(9)));
    assert_ne!(
        format!("{}:{}", a.transaction_hash, a.event_type),
        format!("{}:{}", b.transaction_hash, b.event_type)
    );
}

#[test]
fn initial_cursor_starts_backfill_hours_ago() {
    let c = initial_cursor(48);
    let age = Utc::now().naive_utc() - c.inserted_at;
    assert!(age >= ChronoDuration::hours(48) && age < ChronoDuration::hours(48) + ChronoDuration::minutes(1));
    assert_eq!(c.id, Uuid::nil());
}

// DB-backed: the dev rec DB and Elixir DB share one database (same as
// recorder::tests). Skips without DATABASE_URL. Reads only — never touches
// the singleton cursor row a running service may own.
#[tokio::test]
async fn fetch_page_reads_purchases_after_the_cursor_in_order() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let pool = PgPool::connect(&url).await.expect("connect");

    let creator = format!("0x{}", Uuid::new_v4().simple());
    let nft_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO nfts (id, contract_address, contract_type, creator_address, owner_address, inserted_at, updated_at) \
         VALUES ($1, '0xpurchasesignaltestpurchasesignaltest00', 'art', $2, $2, NOW(), NOW())",
    )
    .bind(nft_id)
    .bind(&creator)
    .execute(&pool)
    .await
    .expect("insert nft");

    let t0 = (Utc::now() - ChronoDuration::seconds(10)).naive_utc();
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    for (id, secs) in [(first, 1i64), (second, 2)] {
        sqlx::query(
            "INSERT INTO purchases (id, buyer_address, tx_hash, block_number, nft_id, inserted_at, updated_at) \
             VALUES ($1, $2, $3, 1, $4, $5, $5)",
        )
        .bind(id)
        .bind(format!("0x{}", Uuid::new_v4().simple()))
        .bind(format!("0x{}", Uuid::new_v4().simple()))
        .bind(nft_id)
        .bind(t0 + ChronoDuration::seconds(secs))
        .execute(&pool)
        .await
        .expect("insert purchase");
    }

    let page = fetch_page(&pool, Cursor { inserted_at: t0, id: Uuid::nil() }).await.expect("fetch");
    let ours: Vec<_> = page.iter().filter(|r| r.nft_uuid == nft_id).collect();
    assert_eq!(ours.iter().map(|r| r.id).collect::<Vec<_>>(), vec![first, second]);
    assert_eq!(ours[0].contract_type.as_deref(), Some("art"));

    // Strictly after: a cursor at the first row returns only the second.
    let after_first = Cursor { inserted_at: ours[0].inserted_at, id: first };
    let page = fetch_page(&pool, after_first).await.expect("fetch");
    assert!(page.iter().filter(|r| r.nft_uuid == nft_id).all(|r| r.id == second));

    sqlx::query("DELETE FROM nfts WHERE id = $1").bind(nft_id).execute(&pool).await.ok();
}
