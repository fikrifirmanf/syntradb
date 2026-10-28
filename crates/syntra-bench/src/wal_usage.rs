use crate::Result;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalUsage {
    pub records: u64,
    pub full_page_images: u64,
    pub bytes: u64,
}
impl WalUsage {
    /// Root instrumentation already includes child work. Never sum plan nodes.
    pub fn from_explain(explain: &Value) -> Result<Self> {
        let plan = explain
            .get(0)
            .and_then(|v| v.get("Plan"))
            .ok_or("missing root plan")?;
        let field = |name| {
            plan.get(name)
                .and_then(Value::as_u64)
                .ok_or("missing or invalid root WAL counter")
        };
        Ok(Self {
            records: field("WAL Records")?,
            full_page_images: field("WAL FPI")?,
            bytes: field("WAL Bytes")?,
        })
    }
    pub fn report(self) -> Value {
        json!({"records":self.records,"full_page_images":self.full_page_images,"bytes":self.bytes})
    }
}
