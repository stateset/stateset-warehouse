//! Retain original work ownership, execution and task history through pg_restore.
use super::*;
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use stateset_warehouse::{api::App, integrations::Connections, work_tasks};

pub(super) async fn seed(store: &Store) -> Result<Value> {
    command(store, "restore-work-product", serde_json::from_value(json!({"type":"create_product","sku":"RESTORE-WORK","name":"Synthetic putaway","category":"Synthetic","reorder_point":0}))?).await?;
    let (_, receipt) = command(
        store,
        "restore-work-receipt",
        Command::CreateReceipt {
            reference: "RESTORE-WORK-RCV".into(),
            supplier: "Synthetic".into(),
            lines: vec![ReceiptInput {
                id: "WORK-LINE".into(),
                sku: "RESTORE-WORK".into(),
                lot: "WORK-LOT".into(),
                quantity: 1,
            }],
        },
    )
    .await?;
    command(
        store,
        "restore-work-receive",
        Command::Receive {
            receipt_id: entity(&receipt)?,
            line_id: "WORK-LINE".into(),
            quantity: 1,
            location: "RECV-01".into(),
        },
    )
    .await?;
    let task: Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM wms_work_tasks t WHERE tenant=$1 AND warehouse_id=$2 AND sku='RESTORE-WORK'").bind(TENANT).bind(WAREHOUSE).fetch_one(&store.pool).await?;
    let id = task["id"]
        .as_str()
        .ok_or("Missing work identity")?
        .to_string();
    let app = App {
        store: store.clone(),
        credentials: std::sync::Arc::new(vec![]),
        demo: false,
        connections: Connections::default(),
    };
    let claimed = application(
        work_tasks::action(
            State(app.clone()),
            Extension(identity()),
            Path((WAREHOUSE.into(), id.clone())),
            Json(serde_json::from_value(
                json!({"revision":task["revision"],"action":"claim"}),
            )?),
        )
        .await,
    )?
    .0;
    let reviewed = application(
        work_tasks::review(
            State(app),
            Extension(identity()),
            Path((WAREHOUSE.into(), id.clone())),
            Json(serde_json::from_value(
                json!({"revision":claimed["revision"]}),
            )?),
        )
        .await,
    )?
    .0;
    let request: Request = serde_json::from_value(reviewed["request"].clone())?;
    let response = application(store.execute(&identity(), WAREHOUSE, &request).await)?;
    Ok(json!({"task_id":id,"request":request,"response":response}))
}

pub(super) async fn verify(store: &Store, original: &Value) -> Result<()> {
    let request: Request = serde_json::from_value(original["request"].clone())?;
    let before = table_evidence(store).await?;
    let replay = application(store.execute(&identity(), WAREHOUSE, &request).await)?;
    require(
        replay["replayed"] == true && replay["result"] == original["response"]["result"],
        "Restored putaway must replay its original physical receipt",
    )?;
    require(
        table_evidence(store).await? == before,
        "Putaway replay changed inventory or task history after restore",
    )?;
    let completed: bool=sqlx::query_scalar("SELECT status='completed' AND completed_quantity=quantity FROM wms_work_tasks WHERE tenant=$1 AND warehouse_id=$2 AND id=$3").bind(TENANT).bind(WAREHOUSE).bind(original["task_id"].as_str()).fetch_one(&store.pool).await?;
    require(completed, "Restored task lost its completed progress")
}
