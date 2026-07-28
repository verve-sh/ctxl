#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ctxl::{db, store};
use rusqlite::Connection;

fn in_memory_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open_in_memory");
    db::apply_schema(&conn).expect("apply_schema");
    conn
}

// @ac AC-1421-04
#[test]
fn token_counting_via_tokenx_rs() {
    // Verify: `store::write` populates the existing `handles.token_est` column using `tokenx-rs` heuristic estimation (column exists in schema but is currently NULL)
    // Verify: The estimate is stored in the DB and included in block message metadata

    let conn = in_memory_conn();

    // Write a handle with known content
    let content = "fn main() {\n    println!(\"Hello, world!\");\n}\n";
    let payload = serde_json::json!({
        "tool": "Bash",
        "output_mode": "stdout",
        "cwd": "/tmp/test",
        "content": content
    });

    let id = store::write(&conn, payload).expect("write should succeed");

    // Read back token_est from the database
    let token_est: Option<i64> = conn
        .query_row("SELECT token_est FROM handles WHERE id = ?1", [&id], |row| row.get(0))
        .expect("row should exist");

    // token_est must be populated (not NULL) and positive
    assert!(token_est.is_some(), "token_est should be populated, not NULL");
    let est = token_est.unwrap();
    assert!(est > 0, "token_est should be positive, got {est}");

    // Sanity check: ~15-30 tokens for a small Rust snippet
    // tokenx-rs estimates ~4 chars/token on average
    let approx_max = (content.len() as i64) / 2;
    assert!(
        est <= approx_max,
        "token_est ({est}) should be ≤ half of byte length ({approx_max}) — sounds too high"
    );

    // Also verify the `token_estimate` helper function directly
    let direct = store::token_estimate(content);
    assert_eq!(direct, est, "store::token_estimate() should return the same value as was stored");

    // Verify a longer document gets a higher estimate than a short one
    let long_content: String = "fn process() {}\n".repeat(100);
    let long_est = store::token_estimate(&long_content);
    let short_est = store::token_estimate("fn f() {}");
    assert!(
        long_est > short_est,
        "longer content ({long_est}) should have higher token estimate than short ({short_est})"
    );
}
