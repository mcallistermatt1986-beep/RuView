//! `gamma-clinic` — serve the ADR-251 clinical dashboard over a store file.
//!
//! Usage: `gamma-clinic [STORE_PATH] [BIND_ADDR]`
//! Defaults: `clinic.jsonl`, `127.0.0.1:8090`. Read-only surface.

use std::sync::Arc;

use tokio::sync::RwLock;

use ruview_gamma_clinic::server::router;
use ruview_gamma_clinic::store::ClinicStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let store_path = args.next().unwrap_or_else(|| "clinic.jsonl".to_string());
    let bind = args.next().unwrap_or_else(|| "127.0.0.1:8090".to_string());

    // Fails closed on a tampered chain — refuses to serve doctored data.
    let store = ClinicStore::open(&store_path)?;
    let status = store.verify_chain();
    println!(
        "gamma-clinic: store={store_path} records={} chain={}",
        status.records,
        if status.valid { "ok" } else { "BROKEN" }
    );

    let app = router(Arc::new(RwLock::new(store)));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("gamma-clinic: dashboard at http://{bind}/ (read-only; research use only)");
    axum::serve(listener, app).await?;
    Ok(())
}
