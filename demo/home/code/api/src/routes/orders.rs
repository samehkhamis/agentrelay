/// GET /orders?cursor=&limit= — newest first, stable across inserts.
pub async fn list(q: Query<Page>, db: Db) -> Json<OrdersPage> {
    let limit = q.limit.unwrap_or(50).min(200);
    let rows = db.orders_before(q.cursor, limit + 1).await;
    let next_cursor = (rows.len() > limit).then(|| rows[limit - 1].cursor());
    Json(OrdersPage { orders: rows.into_iter().take(limit).collect(), next_cursor })
}
