use crate::Moment;

id!(
    /// One submission of `dibs batch`, which `dibs --kill <batch-id>` stops.
    BatchId
);

impl BatchId {
    /// A new batch's: the time it starts here, then this process's id.
    pub fn starting() -> BatchId {
        BatchId(format!(
            "{}-{}",
            Moment::now().to_dashed(),
            std::process::id()
        ))
    }
}
